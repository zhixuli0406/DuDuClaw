//! WP-G2 — per-criterion acceptance ledger for the goal loop.
//!
//! A goal's acceptance criteria are frozen at creation
//! (`acceptance_criteria_baseline`, H9-G) as one block of newline-separated
//! text. The judge is told to check them "item by item", but nothing a
//! program can read showed that every item was actually handled. This module
//! turns the frozen baseline into one [`CriterionUnit`] per non-empty line,
//! lets the worker report a status per unit through a strict
//! `<criteria_status>` reply tag, and gives the judge and the human the same
//! ledger to look at.
//!
//! Design: `commercial/docs/DESIGN-llm-contract-goal-loop-2026-10.md` §2
//! "WP-G2" and its "實作前修訂" subsection (normative where they differ).
//!
//! Rules carried over from the `llm_contract` primitives:
//!
//! - **The model never invents an id.** The system numbers the criteria
//!   `C1..Cn` (the short handle the model sees and echoes back) and derives
//!   the stable `id` itself from `canonical_id([task_id, "criterion", index,
//!   Fingerprint(text)])`. The baseline is frozen, so handles never move.
//! - **A void report is never repaired.** The tag body must be exactly one
//!   JSON array ([`strict_json::parse_strict_with_limit`], fields
//!   `deny_unknown_fields`), every known handle exactly once, statuses limited
//!   to `covered|blocked|candidate`, and every evidence / unresolved string
//!   visible text. Anything else voids the whole round's report: the ledger
//!   stays as it was and the invalid-report counter goes up.
//! - **Immutable updates.** [`apply_report`] builds a new `Vec`; nothing here
//!   mutates a ledger in place.
//!
//! State table (shared by [`validate_unit`] and the report field rules):
//!
//! | status      | evidence  | unresolved |
//! |-------------|-----------|------------|
//! | `planned`   | empty     | empty      |
//! | `covered`   | non-empty | empty      |
//! | `blocked`   | any       | non-empty  |
//! | `candidate` | non-empty | any        |

use std::fmt;
use std::path::Path;

use duduclaw_core::llm_contract::fingerprint::{Fingerprint, canonical_id};
use duduclaw_core::llm_contract::strict_json::{self, Violation};
use duduclaw_core::llm_contract::visible_text::is_visible_text;
use serde::{Deserialize, Serialize};

use super::state::xml_escape;

/// Most criteria one ledger tracks. Extra baseline lines are folded into the
/// last unit's text (with a note), never dropped.
pub const MAX_CRITERIA: usize = 20;
/// Per-entry cap for an evidence / unresolved string (chars, after trimming).
pub const MAX_ENTRY_CHARS: usize = 500;
/// Most evidence or unresolved entries one criterion report may carry.
pub const MAX_ENTRIES: usize = 8;
/// Byte cap for a `<criteria_status>` tag body.
pub const MAX_REPORT_BYTES: usize = 64 * 1024;
/// Reply tag the worker reports through.
pub const TAG_OPEN: &str = "<criteria_status>";
pub const TAG_CLOSE: &str = "</criteria_status>";
/// Audit event for a present-but-invalid report.
pub const CRITERIA_STATUS_INVALID_EVENT: &str = "criteria_status_invalid";
/// How many (masked) characters of an invalid tag body the audit event keeps.
pub const AUDIT_BODY_HEAD_CHARS: usize = 200;
/// Cap on one `unresolved` string inside the needs_human summary.
const SUMMARY_UNRESOLVED_CHARS: usize = 80;
/// Cap on the whole needs_human summary.
const SUMMARY_MAX_CHARS: usize = 600;

// ── Mode ────────────────────────────────────────────────────────────────

/// `config.toml [goal_loop] criteria_ledger`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriteriaLedgerMode {
    /// No ledger is created, injected, parsed or shown to the judge.
    Off,
    /// Ledger created, injected, parsed and shown to humans; the judge gets
    /// it as reference only and its output contract is unchanged.
    #[default]
    Report,
    /// Report, plus the judge panel must return a verdict per criterion.
    Enforce,
}

impl CriteriaLedgerMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Report => "report",
            Self::Enforce => "enforce",
        }
    }

    /// Lenient: only `"off"` and `"enforce"` (trimmed, ASCII
    /// case-insensitive) move away from the default; an absent key, a
    /// non-string value or an unknown string is [`Self::Report`]. Report
    /// never changes a verdict, so a typo costs nothing but strictness.
    pub fn from_value(value: Option<&toml::Value>) -> Self {
        match value
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("off") => Self::Off,
            Some("enforce") => Self::Enforce,
            _ => Self::Report,
        }
    }

    /// Read the mode from `<home_dir>/config.toml` at each decision (hot
    /// reload, same schedule as `[dispatch] strict_reply_parsing`).
    ///
    /// `home_dir = None` ⇒ [`Self::Off`]: a component built without a home
    /// (tests, legacy construction paths) keeps its pre-WP-G2 behaviour
    /// exactly. A home whose `config.toml` is missing, unreadable or
    /// malformed ⇒ [`Self::Report`] (the default).
    pub fn from_home(home_dir: Option<&Path>) -> Self {
        let Some(home_dir) = home_dir else {
            return Self::Off;
        };
        let Ok(content) = std::fs::read_to_string(home_dir.join("config.toml")) else {
            return Self::default();
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        Self::from_value(
            table
                .get("goal_loop")
                .and_then(|v| v.as_table())
                .and_then(|s| s.get("criteria_ledger")),
        )
    }
}

// ── Units ───────────────────────────────────────────────────────────────

/// Closed status set (serde snake_case).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionStatus {
    Planned,
    Covered,
    Blocked,
    Candidate,
}

impl CriterionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Covered => "covered",
            Self::Blocked => "blocked",
            Self::Candidate => "candidate",
        }
    }

    /// zh-TW label used in prompts and notification cards.
    pub fn label_zh(self) -> &'static str {
        match self {
            Self::Planned => "尚未回報",
            Self::Covered => "已回報達成",
            Self::Blocked => "受阻",
            Self::Candidate => "自認完成、待確認",
        }
    }
}

/// One acceptance criterion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriterionUnit {
    /// Stable id: `canonical_id([task_id, "criterion", index, fingerprint])`.
    pub id: String,
    /// Short handle the model sees and echoes back (`C1..Cn`).
    pub handle: String,
    /// The criterion text as frozen in the baseline.
    pub text: String,
    pub status: CriterionStatus,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub unresolved: Vec<String>,
    /// Round of the report that last set this unit; `None` while `planned`.
    #[serde(default)]
    pub updated_round: Option<i64>,
}

/// The persisted ledger (`tasks.criteria_ledger`, and a snapshot per round on
/// `task_iterations.criteria_ledger_json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriteriaLedger {
    /// Mode in effect when the ledger was last written.
    pub mode: CriteriaLedgerMode,
    pub units: Vec<CriterionUnit>,
    #[serde(default)]
    pub last_report_round: Option<i64>,
    #[serde(default)]
    pub invalid_reports: u32,
}

impl CriteriaLedger {
    /// A fresh ledger for a new goal. `None` when the baseline yields no
    /// criterion (empty / whitespace-only) — no ledger is created then.
    pub fn new(task_id: &str, baseline: &str, mode: CriteriaLedgerMode) -> Option<Self> {
        let units = build_ledger(task_id, baseline);
        (!units.is_empty()).then_some(Self {
            mode,
            units,
            last_report_round: None,
            invalid_reports: 0,
        })
    }

    /// Parse a stored ledger. Missing / malformed / a unit that breaks the
    /// state table ⇒ `None` — callers then behave as for a goal without a
    /// ledger (no block, no parsing) rather than act on a corrupt one.
    pub fn from_json(raw: Option<&str>) -> Option<Self> {
        let ledger: Self = serde_json::from_str(raw?).ok()?;
        if ledger.units.is_empty() || ledger.units.iter().any(|u| validate_unit(u).is_err()) {
            return None;
        }
        Some(ledger)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "null".to_string())
    }

    pub fn handles(&self) -> Vec<String> {
        self.units.iter().map(|u| u.handle.clone()).collect()
    }

    /// The task-detail RPC shape (`criteria_ledger` in `tasks.timeline`).
    /// `mode` is the mode in effect now, so the page shows what applies.
    pub fn to_rpc_json(&self, current_mode: CriteriaLedgerMode) -> serde_json::Value {
        serde_json::json!({
            "mode": current_mode.as_str(),
            "units": self.units.iter().map(|u| serde_json::json!({
                "id": u.id,
                "handle": u.handle,
                "text": u.text,
                "status": u.status.as_str(),
                "evidence": u.evidence,
                "unresolved": u.unresolved,
                "updated_round": u.updated_round,
            })).collect::<Vec<_>>(),
            "last_report_round": self.last_report_round,
            "invalid_reports": self.invalid_reports,
        })
    }
}

/// The ONE place a new goal's ledger is decided, shared by every
/// user-initiated creation path (`goal_create_core` for the dashboard and
/// MCP, chat `/goal`, and the goal-intent "想一想" confirmation). Reads
/// `[goal_loop] criteria_ledger` from `home_dir`; returns the JSON to store
/// in `tasks.criteria_ledger`, or `None` when the mode is `off` or the
/// baseline yields no criterion. Callers set it on the row before
/// `insert_task`, so there is no second write. Autopilot and planner
/// sub-tasks deliberately do not call this and have no ledger.
pub fn ledger_for_new_goal(home_dir: &Path, task_id: &str, baseline: Option<&str>) -> Option<String> {
    let mode = CriteriaLedgerMode::from_home(Some(home_dir));
    if mode == CriteriaLedgerMode::Off {
        return None;
    }
    CriteriaLedger::new(task_id, baseline?, mode).map(|l| l.to_json())
}

/// One unit per non-empty trimmed baseline line, handles `C1..Cn` in order,
/// all `planned`. More than [`MAX_CRITERIA`] lines: the extra lines are
/// folded into the last unit's text after a note. A line whose id cannot be
/// built (cannot happen for a real task id; fail closed) yields an empty
/// ledger, i.e. no ledger at all rather than a partial one.
pub fn build_ledger(task_id: &str, baseline_text: &str) -> Vec<CriterionUnit> {
    let lines: Vec<&str> = baseline_text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let mut texts: Vec<String> = lines
        .iter()
        .take(MAX_CRITERIA)
        .map(|l| (*l).to_string())
        .collect();
    if lines.len() > MAX_CRITERIA {
        let extra = &lines[MAX_CRITERIA..];
        if let Some(last) = texts.last_mut() {
            *last = format!(
                "{last}\n（驗收標準超過 {MAX_CRITERIA} 條，以下 {} 條併入本條一起檢核）\n{}",
                extra.len(),
                extra.join("\n")
            );
        }
    }
    let mut units = Vec::with_capacity(texts.len());
    for (i, text) in texts.into_iter().enumerate() {
        let index = (i + 1).to_string();
        let fingerprint = Fingerprint::derive(&[text.as_str()]);
        let Ok(id) = canonical_id(&[task_id, "criterion", &index, fingerprint.as_str()]) else {
            return Vec::new();
        };
        units.push(CriterionUnit {
            id,
            handle: format!("C{index}"),
            text,
            status: CriterionStatus::Planned,
            evidence: Vec::new(),
            unresolved: Vec::new(),
            updated_round: None,
        });
    }
    units
}

/// Why a unit breaks the state table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnitError {
    MissingId,
    BadHandle(String),
    EmptyText,
    /// `field` is `evidence` or `unresolved`.
    InvalidEntry {
        field: &'static str,
    },
    TooManyEntries {
        field: &'static str,
    },
    StatusRule {
        status: CriterionStatus,
        rule: &'static str,
    },
}

impl fmt::Display for UnitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingId => write!(f, "criterion unit has no id"),
            Self::BadHandle(h) => write!(f, "criterion handle `{h}` is not of the form C<n>"),
            Self::EmptyText => write!(f, "criterion text has no visible content"),
            Self::InvalidEntry { field } => {
                write!(
                    f,
                    "`{field}` has an empty, untrimmed, invisible or over-long entry"
                )
            }
            Self::TooManyEntries { field } => {
                write!(f, "`{field}` has more than {MAX_ENTRIES} entries")
            }
            Self::StatusRule { status, rule } => {
                write!(f, "status `{}` requires {rule}", status.as_str())
            }
        }
    }
}

fn is_handle(h: &str) -> bool {
    h.strip_prefix('C').is_some_and(|n| {
        !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && !n.starts_with('0')
    })
}

fn check_entries(field: &'static str, entries: &[String]) -> Result<(), UnitError> {
    if entries.len() > MAX_ENTRIES {
        return Err(UnitError::TooManyEntries { field });
    }
    if entries.iter().any(|e| !is_visible_text(e, MAX_ENTRY_CHARS)) {
        return Err(UnitError::InvalidEntry { field });
    }
    Ok(())
}

/// The status/field rule alone (shared by units and incoming reports).
fn check_status_rule(
    status: CriterionStatus,
    evidence: &[String],
    unresolved: &[String],
) -> Result<(), UnitError> {
    let rule = match status {
        CriterionStatus::Planned if !evidence.is_empty() || !unresolved.is_empty() => {
            Some("both `evidence` and `unresolved` to be empty")
        }
        CriterionStatus::Covered if evidence.is_empty() => Some("a non-empty `evidence`"),
        CriterionStatus::Covered if !unresolved.is_empty() => Some("an empty `unresolved`"),
        CriterionStatus::Blocked if unresolved.is_empty() => Some("a non-empty `unresolved`"),
        CriterionStatus::Candidate if evidence.is_empty() => Some("a non-empty `evidence`"),
        _ => None,
    };
    match rule {
        Some(rule) => Err(UnitError::StatusRule { status, rule }),
        None => Ok(()),
    }
}

/// Enforce the state table on one unit.
pub fn validate_unit(unit: &CriterionUnit) -> Result<(), UnitError> {
    if unit.id.trim().is_empty() {
        return Err(UnitError::MissingId);
    }
    if !is_handle(&unit.handle) {
        return Err(UnitError::BadHandle(unit.handle.clone()));
    }
    if !duduclaw_core::llm_contract::visible_text::has_visible_content(&unit.text) {
        return Err(UnitError::EmptyText);
    }
    check_entries("evidence", &unit.evidence)?;
    check_entries("unresolved", &unit.unresolved)?;
    check_status_rule(unit.status, &unit.evidence, &unit.unresolved)
}

// ── Rendering ───────────────────────────────────────────────────────────

/// One criterion's text on a single line (folded units span several lines).
fn one_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" / ")
}

/// The `## 驗收帳本` block injected into every goal dispatch prompt.
pub fn render_ledger_block(units: &[CriterionUnit]) -> String {
    let lines = units
        .iter()
        .map(|u| {
            format!(
                "[{}] {} — {}",
                u.handle,
                one_line(&u.text),
                u.status.label_zh()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let first = units.first().map(|u| u.handle.as_str()).unwrap_or("C1");
    format!(
        "## 驗收帳本\n\
         驗收標準已由系統逐條編號，代號由系統產生，回報時請原樣使用，不要自己編代號：\n\
         {lines}\n\n\
         回報方式：在 `tasks_complete` 的 result_summary 最後附上下面這種驗收回報標記（開頭與結尾標籤都要有）。\
         標記內只能有一個 JSON 陣列（前後不可夾雜其他文字），上面每個代號恰好出現一次：\n\
         <criteria_status>[{{\"id\": \"{first}\", \"status\": \"covered\", \"evidence\": \
         [\"產出檔 reports/summary.md\"], \"unresolved\": []}}]</criteria_status>\n\
         status 只能是 covered（已達成：evidence 必填、unresolved 必須是空陣列）、\
         blocked（受阻：unresolved 必填，寫清楚卡在哪）、candidate（自認完成但需要確認：evidence 必填）。\
         evidence 寫工具呼叫或產出檔路徑；每項最多 {MAX_ENTRY_CHARS} 字、每個欄位最多 {MAX_ENTRIES} 項。\
         格式不符時，這一輪的回報整筆作廢，帳本維持原狀。這份回報是你的自述，驗收判官仍會依實際產出判定。"
    )
}

/// The dispatch-prompt section for a task: `None` when the mode is `off`
/// or the task has no (readable) ledger, so such a payload is unchanged.
pub fn dispatch_section(stored: Option<&str>, mode: CriteriaLedgerMode) -> Option<String> {
    if mode == CriteriaLedgerMode::Off {
        return None;
    }
    CriteriaLedger::from_json(stored).map(|l| render_ledger_block(&l.units))
}

/// The worker's self-reported ledger as a judge-prompt reference block.
/// Every interpolated string is XML-escaped; the wording marks the content
/// as self-report, not evidence.
pub fn render_judge_reference(units: &[CriterionUnit]) -> String {
    let mut body = String::new();
    for u in units {
        body.push_str(&format!(
            "[{}] {} — {}\n",
            u.handle,
            xml_escape(&one_line(&u.text)),
            u.status.as_str()
        ));
        for e in &u.evidence {
            body.push_str(&format!("  evidence: {}\n", xml_escape(e)));
        }
        for r in &u.unresolved {
            body.push_str(&format!("  unresolved: {}\n", xml_escape(r)));
        }
    }
    format!(
        "<criteria_ledger_self_report>\n\
         以下是執行者自己回報的逐條狀態（WORKER SELF-REPORT），只是自述，不是證據；\
         每一條仍請依實際產出、<tool_activity> 與產物收據判定。\n\
         {body}</criteria_ledger_self_report>"
    )
}

/// Compact needs_human summary: which criteria are blocked / still planned /
/// awaiting confirmation, with their `unresolved` strings. `None` when every
/// criterion is reported covered or the ledger is empty.
pub fn needs_human_summary(units: &[CriterionUnit]) -> Option<String> {
    let open: Vec<String> = units
        .iter()
        .filter(|u| u.status != CriterionStatus::Covered)
        .map(|u| {
            let detail = u
                .unresolved
                .iter()
                .map(|r| duduclaw_core::truncate_chars(r, SUMMARY_UNRESOLVED_CHARS))
                .collect::<Vec<_>>()
                .join("、");
            if detail.is_empty() {
                format!("{} {}", u.handle, u.status.label_zh())
            } else {
                format!("{} {}（{detail}）", u.handle, u.status.label_zh())
            }
        })
        .collect();
    if open.is_empty() {
        return None;
    }
    let covered = units.len() - open.len();
    let text = format!(
        "驗收帳本：{covered}/{} 條已回報達成；{}",
        units.len(),
        open.join("；")
    );
    Some(duduclaw_core::truncate_chars(&text, SUMMARY_MAX_CHARS))
}

// ── Report parsing ──────────────────────────────────────────────────────

/// What the worker may report (no `planned`: a reported unit has moved).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReportStatus {
    Covered,
    Blocked,
    Candidate,
}

impl From<ReportStatus> for CriterionStatus {
    fn from(s: ReportStatus) -> Self {
        match s {
            ReportStatus::Covered => Self::Covered,
            ReportStatus::Blocked => Self::Blocked,
            ReportStatus::Candidate => Self::Candidate,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReport {
    id: String,
    status: ReportStatus,
    #[serde(default)]
    evidence: Vec<String>,
    #[serde(default)]
    unresolved: Vec<String>,
}

/// One validated per-criterion report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriterionReport {
    pub handle: String,
    pub status: CriterionStatus,
    pub evidence: Vec<String>,
    pub unresolved: Vec<String>,
}

/// Why a `<criteria_status>` report was voided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CriteriaReportError {
    /// The tag body is not exactly one JSON array of report objects.
    Contract(Violation),
    /// The reply carried more than one `<criteria_status>` tag.
    MultipleTags,
    /// An opening tag with no closing tag.
    Unterminated,
    UnknownHandle(String),
    DuplicateHandle(String),
    MissingHandle(String),
    Unit {
        handle: String,
        error: UnitError,
    },
}

impl fmt::Display for CriteriaReportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contract(v) => write!(f, "criteria_status contract violation: {v}"),
            Self::MultipleTags => write!(f, "reply carries more than one <criteria_status> tag"),
            Self::Unterminated => write!(f, "<criteria_status> tag is not closed"),
            Self::UnknownHandle(h) => write!(f, "unknown criterion handle `{h}`"),
            Self::DuplicateHandle(h) => write!(f, "criterion handle `{h}` reported more than once"),
            Self::MissingHandle(h) => write!(f, "criterion handle `{h}` missing from the report"),
            Self::Unit { handle, error } => write!(f, "criterion `{handle}`: {error}"),
        }
    }
}

impl std::error::Error for CriteriaReportError {}

/// Trim, cut to [`MAX_ENTRY_CHARS`] (char-safe), trim again. The visibility
/// check happens afterwards in [`check_entries`].
fn normalize_entries(entries: Vec<String>) -> Vec<String> {
    entries
        .into_iter()
        .map(|e| {
            duduclaw_core::truncate_chars(e.trim(), MAX_ENTRY_CHARS)
                .trim()
                .to_string()
        })
        .collect()
}

/// Parse a `<criteria_status>` tag body against the ledger's handles.
pub fn parse_criteria_status(
    raw_tag_body: &str,
    known_handles: &[String],
) -> Result<Vec<CriterionReport>, CriteriaReportError> {
    let raw: Vec<RawReport> = strict_json::parse_strict_with_limit(raw_tag_body, MAX_REPORT_BYTES)
        .map_err(CriteriaReportError::Contract)?;
    let mut out: Vec<CriterionReport> = Vec::with_capacity(raw.len());
    for r in raw {
        let handle = r.id.trim().to_string();
        if !known_handles.contains(&handle) {
            return Err(CriteriaReportError::UnknownHandle(
                duduclaw_core::truncate_chars(&handle, 40),
            ));
        }
        if out.iter().any(|o| o.handle == handle) {
            return Err(CriteriaReportError::DuplicateHandle(handle));
        }
        let status = CriterionStatus::from(r.status);
        let unit_err = |error| CriteriaReportError::Unit {
            handle: handle.clone(),
            error,
        };
        if r.evidence.len() > MAX_ENTRIES {
            return Err(unit_err(UnitError::TooManyEntries { field: "evidence" }));
        }
        if r.unresolved.len() > MAX_ENTRIES {
            return Err(unit_err(UnitError::TooManyEntries {
                field: "unresolved",
            }));
        }
        let evidence = normalize_entries(r.evidence);
        let unresolved = normalize_entries(r.unresolved);
        check_entries("evidence", &evidence).map_err(unit_err)?;
        check_entries("unresolved", &unresolved).map_err(unit_err)?;
        check_status_rule(status, &evidence, &unresolved).map_err(unit_err)?;
        out.push(CriterionReport {
            handle,
            status,
            evidence,
            unresolved,
        });
    }
    if let Some(missing) = known_handles
        .iter()
        .find(|h| !out.iter().any(|o| &o.handle == *h))
    {
        return Err(CriteriaReportError::MissingHandle(missing.clone()));
    }
    Ok(out)
}

/// The ledger after a valid report: every reported unit takes the reported
/// status/evidence/unresolved and `updated_round = round`. Built as a new
/// `Vec`; the input is untouched.
pub fn apply_report(
    units: &[CriterionUnit],
    reports: &[CriterionReport],
    round: i64,
) -> Vec<CriterionUnit> {
    units
        .iter()
        .map(|u| match reports.iter().find(|r| r.handle == u.handle) {
            Some(r) => CriterionUnit {
                status: r.status,
                evidence: r.evidence.clone(),
                unresolved: r.unresolved.clone(),
                updated_round: Some(round),
                ..u.clone()
            },
            None => u.clone(),
        })
        .collect()
}

// ── Reply-tag handling ──────────────────────────────────────────────────

/// What a worker reply carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagExtract {
    Absent,
    /// Exactly one complete tag; the body between the markers.
    One(String),
    /// A tag was present but cannot be read (several tags / unterminated).
    Broken(CriteriaReportError),
}

/// Find the `<criteria_status>` tag(s) in a worker reply.
pub fn extract_tag(text: &str) -> TagExtract {
    let mut bodies: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(TAG_OPEN) {
        let after = &rest[start + TAG_OPEN.len()..];
        let Some(end) = after.find(TAG_CLOSE) else {
            return TagExtract::Broken(CriteriaReportError::Unterminated);
        };
        bodies.push(after[..end].to_string());
        rest = &after[end + TAG_CLOSE.len()..];
    }
    match bodies.len() {
        0 => TagExtract::Absent,
        1 => TagExtract::One(bodies.remove(0)),
        _ => TagExtract::Broken(CriteriaReportError::MultipleTags),
    }
}

/// The reply with every `<criteria_status>…</criteria_status>` removed (an
/// unterminated opening tag removes everything after it). Text without a tag
/// is returned unchanged, byte for byte.
pub fn strip_tag(text: &str) -> String {
    if !text.contains(TAG_OPEN) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(TAG_OPEN) {
        out.push_str(&rest[..start]);
        let after = &rest[start + TAG_OPEN.len()..];
        match after.find(TAG_CLOSE) {
            Some(end) => rest = &after[end + TAG_CLOSE.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim_end().to_string()
}

/// The worker reply as it may be shown or stored for a task: tag removed
/// when the task has a ledger, untouched (byte for byte) otherwise. Used by
/// every display copy (`tasks.*` JSON, the ✅ push) so a reply read before
/// the settle has rewritten the stored text still shows no tag.
pub fn display_result_summary(
    criteria_ledger: Option<&str>,
    result_summary: Option<&str>,
) -> Option<String> {
    match (criteria_ledger, result_summary) {
        (Some(_), Some(text)) => Some(strip_tag(text)),
        (_, text) => text.map(str::to_string),
    }
}

/// How one round's report went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportResult {
    /// No tag: the ledger is unchanged and nothing is counted.
    Absent,
    /// A valid report updated the ledger.
    Applied,
    /// A tag was present but invalid: ledger unchanged, counter +1.
    Invalid {
        error: CriteriaReportError,
        /// The raw tag body (empty for a tag that could not be isolated).
        body: String,
    },
}

/// Settle one round: read the tag from `reply`, and return the ledger to
/// persist (a new value; `ledger` is untouched) plus what happened.
pub fn settle_round(
    ledger: &CriteriaLedger,
    reply: &str,
    round: i64,
    mode: CriteriaLedgerMode,
) -> (CriteriaLedger, ReportResult) {
    let handles = ledger.handles();
    let invalid = |error, body: String| {
        (
            CriteriaLedger {
                mode,
                invalid_reports: ledger.invalid_reports.saturating_add(1),
                ..ledger.clone()
            },
            ReportResult::Invalid { error, body },
        )
    };
    match extract_tag(reply) {
        TagExtract::Absent => (
            CriteriaLedger {
                mode,
                ..ledger.clone()
            },
            ReportResult::Absent,
        ),
        TagExtract::Broken(error) => invalid(error, String::new()),
        TagExtract::One(body) => match parse_criteria_status(&body, &handles) {
            Ok(reports) => (
                CriteriaLedger {
                    mode,
                    units: apply_report(&ledger.units, &reports, round),
                    last_report_round: Some(round),
                    invalid_reports: ledger.invalid_reports,
                },
                ReportResult::Applied,
            ),
            Err(error) => invalid(error, body),
        },
    }
}

/// The `criteria_status_invalid` audit event: the violation's Display and at
/// most [`AUDIT_BODY_HEAD_CHARS`] masked characters of the tag body.
pub fn invalid_report_event(
    agent_id: &str,
    task_id: &str,
    round: i64,
    error: &CriteriaReportError,
    body: &str,
) -> duduclaw_security::audit::AuditEvent {
    let masked = duduclaw_security::audit::mask_sensitive_text(body);
    duduclaw_security::audit::AuditEvent::new(
        CRITERIA_STATUS_INVALID_EVENT,
        agent_id,
        duduclaw_security::audit::Severity::Info,
        serde_json::json!({
            "task_id": task_id,
            "round": round,
            "violation": duduclaw_core::truncate_chars(&error.to_string(), 300),
            "body_head": duduclaw_core::truncate_chars(&masked, AUDIT_BODY_HEAD_CHARS),
            "body_bytes": body.len(),
        }),
    )
}

#[cfg(test)]
mod tests;
