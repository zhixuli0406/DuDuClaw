//! Workspace action claims: mail / calendar / sheets / docs / tasks.
//!
//! First-person completion claims only ("已寄出", "已建立行程", "I've sent
//! the email", ...). A claim is backed by a successful (`success == true`,
//! not `usage_only`) tool-call row of the SAME employee in the SAME session,
//! from the current turn or from the last
//! [`SESSION_EVIDENCE_WINDOW_MINUTES`] minutes. Rows without a session id
//! never count, and a turn without a session id is not evaluated at all
//! (nothing can be proven either way — fail quiet). A mail claim is backed
//! only by a row of a [`MAIL_SENT_EVIDENCE_TOOLS`] tool with `sent == true`.
//!
//! Only the first [`MAX_SCAN_CHARS`] characters of a reply are scanned.
//!
//! Guards are evaluated on the whole sentence containing a match. The reply
//! is split into sentences after each of `。．.!！?？` and newline (the
//! terminator stays with its sentence; commas do NOT end a sentence). A
//! match is a claim only when its sentence
//! - is not a question: it does not end with `?` / `？` and contains none
//!   of 嗎, 是否, 有沒有 anywhere; and
//! - has no negation / condition / future marker anywhere before the match
//!   in that sentence (zh substrings such as 尚未, 如果, 一旦, 將, 才會; English
//!   whole words such as not, once, if, will, could, "going to", "to be").
//!
//! The patterns also keep two immediate guard groups (`neg`: a word such as
//! 不要說 / 未 / be right before the claim; `post`: 後 or 與否 right after
//! it); a match with either group present is not a claim. False negatives
//! are preferred over false positives. Known limit: quoted user text and
//! code blocks are not excluded, so a reply quoting "已寄出" can trigger.

use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use regex::{Regex, RegexSet};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ActionClaim, ClaimType};

/// Rows this many minutes old or newer (same session) back a claim, so a
/// "剛才已建立" re-confirmation of an earlier turn's action is not flagged.
pub const SESSION_EVIDENCE_WINDOW_MINUTES: i64 = 30;

/// Longest wait for the tool-call log lock in
/// [`detect_unbacked_workspace_claims`]. A lock not acquired in time is an
/// error ("unknown"), never "no rows".
pub const SESSION_EVIDENCE_LOCK_WAIT: Duration = Duration::from_secs(2);

/// Most characters of a reply that are scanned for claims. A claim after
/// this point is not detected (bounded regex work per reply).
pub const MAX_SCAN_CHARS: usize = 16_000;

/// The mail tools whose successful row backs a "sent" claim — and only
/// when the row also carries `sent == true`. These are the mail tools in
/// `duduclaw_core::mcp_tool_defs` that can produce a sent mail; today only
/// `mail_send` and confirmed `gmail_send_draft` write `sent` audit fields. No other tool's row
/// backs a mail claim, whatever fields it carries.
const MAIL_SENT_EVIDENCE_TOOLS: &[&str] = &["mail_send", "gmail_create_draft", "gmail_send_draft"];

/// A successful row of one of these without `sent == true` means the mail
/// is only a draft or waits for confirmation.
const MAIL_DRAFT_TOOLS: &[&str] = &["mail_send", "gmail_create_draft"];

/// Longest matched fragment kept on a claim (characters).
const MAX_MATCHED_CHARS: usize = 80;

/// Sentence terminators (kept with the sentence they end).
const SENTENCE_END: &[char] = &['。', '．', '.', '!', '！', '?', '？', '\n'];

/// A sentence containing any of these anywhere is a question.
const ZH_QUESTION_MARKERS: &[&str] = &["嗎", "是否", "有沒有"];

/// zh negation / condition / future markers: one of these anywhere before
/// the match in its sentence ⇒ not a claim.
const ZH_SCOPE_MARKERS: &[&str] = &[
    "尚未", "還沒", "仍未", "並未", "沒有", "不會", "不能", "無法", "是否", "如果", "若", "一旦",
    "等", "待", "將", "會在", "之後", "才會",
];

/// English negation / condition / future words (whole word, any case):
/// one of these anywhere before the match in its sentence ⇒ not a claim.
const EN_SCOPE_WORDS: &[&str] = &[
    "not", "never", "no", "once", "if", "after", "when", "whether", "until", "unless", "will",
    "would", "should", "can", "could",
];

/// English two-word markers (consecutive whole words, any case).
const EN_SCOPE_PAIRS: &[(&str, &str)] = &[("going", "to"), ("be", "sent"), ("to", "be")];

/// zh-TW: a negation / question / "don't say" word right before the claim.
const ZH_NEG: &str = r"(?P<neg>(?:是否|有沒有|尚未|還沒有?|仍未|並未|沒有|不是|並非|不要說|不能說|別說|未|不)\s*)?";
/// zh-TW: a plan ("寄出後") right after the claim. Questions are handled on
/// the whole sentence, not here.
const ZH_POST: &str = r"(?P<post>後|與否)?";
/// zh-TW: optional "for you".
const ZH_FOR: &str = r"(?:幫你|幫您|為你|為您|替你|替您)?";
/// zh-TW: optional short 把-object ("把信", "把這筆資料").
const ZH_BA: &str = r"(?:把[^，,。．.!！?？;；\s]{1,12}?)?";
/// English: a negation / condition / future word right before the claim.
const EN_NEG: &str = r"(?P<neg>\b(?:not|never|once|if|after|when|whether|until|unless|be|will|would)\s+)?";

/// The workspace families, in a stable order (notices list kinds in it).
pub const WORKSPACE_FAMILIES: [ClaimType; 6] = [
    ClaimType::MailSent,
    ClaimType::CalendarEventCreated,
    ClaimType::CalendarEventUpdated,
    ClaimType::SheetWritten,
    ClaimType::DocWritten,
    ClaimType::TaskCreated,
];

struct WorkspacePattern {
    re: Regex,
    claim_type: ClaimType,
}

static WORKSPACE_PATTERNS: LazyLock<Vec<WorkspacePattern>> = LazyLock::new(|| {
    workspace_specs()
        .into_iter()
        .filter_map(|(pat, claim_type)| {
            Regex::new(&pat).ok().map(|re| WorkspacePattern { re, claim_type })
        })
        .collect()
});

/// All workspace patterns as one set: a prefilter so a reply (or sentence)
/// that matches none of them costs one scan, not one per pattern. `None`
/// (never expected; a test proves it compiles) disables the prefilter only.
static WORKSPACE_SET: LazyLock<Option<RegexSet>> =
    LazyLock::new(|| RegexSet::new(workspace_specs().into_iter().map(|(pat, _)| pat)).ok());

/// Whether any workspace pattern may match `text` (true without a prefilter).
fn may_match(text: &str) -> bool {
    WORKSPACE_SET.as_ref().is_none_or(|set| set.is_match(text))
}

/// Pattern source for every workspace family (a test proves each compiles).
fn workspace_specs() -> Vec<(String, ClaimType)> {
    let (n, p, f, ba) = (ZH_NEG, ZH_POST, ZH_FOR, ZH_BA);
    let en = EN_NEG;
    vec![
        // ── Mail sent ────────────────────────────────────────
        (format!(r"{n}已經?\s*{f}\s*{ba}\s*(?:寄出|寄送|發信|寄信){p}"), ClaimType::MailSent),
        (format!(r"{n}(?:信件?|郵件|電子郵件|[Ee]-?mail)\s*(?:也)?已經?\s*(?:成功)?\s*(?:寄出|寄送|發送|送出|寄){p}"), ClaimType::MailSent),
        (format!(r"{n}已經?\s*{f}\s*(?:成功)?\s*(?:發送|送出)\s*(?:了\s*)?(?:這封|那封|該封)?\s*(?:信件?|郵件|電子郵件|[Ee]-?mail){p}"), ClaimType::MailSent),
        (format!(r"(?i){en}\bI(?:['’]ve|\s+have)\s+(?:just\s+|already\s+|now\s+|successfully\s+)?sent\s+(?:the|your|an|that|this|my)\s+(?:e-?mail|mail)\b"), ClaimType::MailSent),
        (format!(r"(?i){en}\b(?:the\s+|your\s+|an?\s+)?e-?mail\s+(?:has\s+been\s+|was\s+|is\s+now\s+)?sent\b"), ClaimType::MailSent),

        // ── Calendar event updated ───────────────────────────
        (format!(r"{n}已經?\s*{f}\s*(?:更新|修改|編輯)\s*(?:了\s*)?(?:行事曆活動|日曆活動|行程|活動|會議){p}"), ClaimType::CalendarEventUpdated),
        (format!(r"(?i){en}\b(?:calendar\s+)?event\s+(?:has\s+been\s+|was\s+)?(?:updated|edited|modified)\b"), ClaimType::CalendarEventUpdated),

        // ── Calendar event created ───────────────────────────
        (format!(r"{n}已經?\s*{f}\s*(?:建立|新增|安排)\s*(?:了\s*)?(?:一個|一筆|這個|新的?)?\s*(?:行事曆活動|日曆活動|行程|活動|會議){p}"), ClaimType::CalendarEventCreated),
        (format!(r"{n}已經?\s*{f}\s*{ba}\s*(?:排入|排進|加入|加到|放入|新增到|新增至|寫入)\s*(?:了\s*)?(?:您的|你的)?\s*(?:Google\s*)?(?:行事曆|日曆|[Cc]alendar){p}"), ClaimType::CalendarEventCreated),
        (format!(r"{n}(?:行程|活動|會議邀請|會議)\s*已經?\s*(?:成功)?\s*(?:建立|新增|排入行事曆|加入行事曆|加到行事曆){p}"), ClaimType::CalendarEventCreated),
        (format!(r"(?i){en}\b(?:the\s+|an?\s+|your\s+)?(?:calendar\s+)?event\s+(?:has\s+been\s+|was\s+)?(?:created|scheduled|added)\b"), ClaimType::CalendarEventCreated),
        (format!(r"(?i){en}\b(?:added|put)\s+(?:(?:it|this|that|them|the\s+(?:event|meeting))\s+)?(?:to|on|in)\s+your\s+(?:Google\s+)?calendar\b"), ClaimType::CalendarEventCreated),
        (format!(r"(?i){en}\bI(?:['’]ve|\s+have)\s+(?:just\s+|already\s+)?(?:created|scheduled|booked)\s+(?:the|an?|your|that|this)\s+(?:calendar\s+)?(?:event|meeting)\b"), ClaimType::CalendarEventCreated),

        // ── Spreadsheet written ──────────────────────────────
        (format!(r"{n}已經?\s*{f}\s*{ba}\s*(?:(?:寫入|填入|更新|記錄|登記|附加|加入|貼上?)(?:到|至|進)?|(?:新增|加)(?:到|至|進))\s*(?:了\s*)?(?:Google\s*)?(?:試算表|工作表|[Ss]heets?\b){p}"), ClaimType::SheetWritten),
        (format!(r"{n}(?:試算表|工作表)\s*已經?\s*(?:成功)?\s*(?:寫入|更新|新增){p}"), ClaimType::SheetWritten),
        (format!(r"(?i){en}\b(?:added|appended|inserted)\s+(?:an?|one|\d+|the)\s+(?:new\s+)?rows?\s+(?:to|in|into)\s+(?:the|your)\s+(?:Google\s+)?(?:spread)?sheet\b"), ClaimType::SheetWritten),
        (format!(r"(?i){en}\b(?:appended|written|wrote|added)\s+(?:(?:it|this|that|them|the\s+(?:data|rows?|entry|entries|records?))\s+)?(?:to|into)\s+(?:the|your)\s+(?:Google\s+)?(?:spread)?sheet\b"), ClaimType::SheetWritten),
        (format!(r"(?i){en}\b(?:the|your)\s+(?:Google\s+)?(?:spread)?sheet\s+(?:has\s+been|was)\s+updated\b"), ClaimType::SheetWritten),

        // ── Google Doc written (narrow) ──────────────────────
        (format!(r"{n}已經?\s*{f}\s*{ba}\s*(?:(?:寫入|更新|附加|貼上?)(?:到|至|進)?|(?:新增|加入|加)(?:到|至|進))\s*(?:了\s*)?(?:(?:Google\s*)?文件|Google\s*[Dd]ocs?\b){p}"), ClaimType::DocWritten),
        (format!(r"(?i){en}\bappended\s+(?:(?:it|this|that|them|the\s+(?:text|notes?|summary|section|content))\s+)?to\s+(?:the|your)\s+(?:Google\s+)?doc(?:ument)?\b"), ClaimType::DocWritten),
        (format!(r"(?i){en}\b(?:added|written|wrote)\s+(?:(?:it|this|that|them)\s+)?to\s+(?:the|your)\s+Google\s+Doc\b"), ClaimType::DocWritten),

        // ── Task created ─────────────────────────────────────
        (format!(r"{n}已經?\s*{f}\s*(?:建立|新增|開立|加入|加到|排入)\s*(?:了\s*)?(?:一個|一筆|這個|新的?)?\s*(?:任務|待辦|工作項目){p}"), ClaimType::TaskCreated),
        (format!(r"{n}(?:任務|待辦事項|待辦)\s*已經?\s*(?:成功)?\s*(?:建立|新增|加入任務看板|加入看板){p}"), ClaimType::TaskCreated),
        (format!(r"(?i){en}\b(?:the\s+|a\s+|your\s+)?(?:new\s+)?task\s+(?:has\s+been\s+|was\s+)?created\b"), ClaimType::TaskCreated),
        (format!(r"(?i){en}\bI(?:['’]ve|\s+have)\s+(?:just\s+|already\s+)?(?:created|added)\s+(?:a|the|your|that|this)\s+(?:new\s+)?task\b"), ClaimType::TaskCreated),
    ]
}

/// Why a workspace claim is not backed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnbackedKind {
    /// Mail only: the session has a draft / pending-confirmation mail row but
    /// nothing was sent.
    DraftOnly,
    /// No evidence row at all.
    NoRecord,
}

/// One workspace claim without a success record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnbackedClaim {
    pub claim: ActionClaim,
    pub kind: UnbackedKind,
    /// Operator-facing explanation (logs / audit only, never shown in chat).
    pub reason: String,
}

/// Whether `sentence` is a question: it ends with `?` / `？` (trailing
/// whitespace ignored) or contains a [`ZH_QUESTION_MARKERS`] word anywhere.
fn is_question(sentence: &str) -> bool {
    let trimmed = sentence.trim_end();
    trimmed.ends_with('?')
        || trimmed.ends_with('？')
        || ZH_QUESTION_MARKERS.iter().any(|m| sentence.contains(*m))
}

/// Whether the part of a sentence before a match carries a negation /
/// condition / future marker ([`ZH_SCOPE_MARKERS`] as substrings,
/// [`EN_SCOPE_WORDS`] / [`EN_SCOPE_PAIRS`] as whole ASCII words, any case).
fn scoped_before(prefix: &str) -> bool {
    if ZH_SCOPE_MARKERS.iter().any(|m| prefix.contains(*m)) {
        return true;
    }
    let lower = prefix.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    words.iter().any(|w| EN_SCOPE_WORDS.contains(w))
        || words.windows(2).any(|pair| EN_SCOPE_PAIRS.contains(&(pair[0], pair[1])))
}

/// Extract workspace claims (at most one per family; `target_id` is empty)
/// from the first [`MAX_SCAN_CHARS`] characters of `output`, with the
/// sentence-level guards described in the module doc. Agent-management
/// claims are not returned here — see [`super::extract_action_claims`].
pub fn extract_workspace_claims(output: &str) -> Vec<ActionClaim> {
    let scanned = duduclaw_core::truncate_chars(output, MAX_SCAN_CHARS);
    let mut claims: Vec<ActionClaim> = Vec::new();
    if !may_match(&scanned) {
        return claims;
    }
    for sentence in scanned.split_inclusive(SENTENCE_END) {
        if claims.len() >= WORKSPACE_FAMILIES.len() {
            break;
        }
        if !may_match(sentence) || is_question(sentence) {
            continue;
        }
        for pattern in WORKSPACE_PATTERNS.iter() {
            if claims.iter().any(|c| c.claim_type == pattern.claim_type) {
                continue;
            }
            for cap in pattern.re.captures_iter(sentence) {
                if cap.name("neg").is_some() || cap.name("post").is_some() {
                    continue;
                }
                let Some(m) = cap.get(0) else { continue };
                // `m.start()` is a char boundary (regex match); if it somehow
                // were not, the whole sentence is the prefix (fail quiet).
                let prefix = sentence.get(..m.start()).unwrap_or(sentence);
                if scoped_before(prefix) {
                    continue;
                }
                claims.push(ActionClaim {
                    claim_type: pattern.claim_type.clone(),
                    target_id: String::new(),
                    matched_text: duduclaw_core::truncate_chars(m.as_str().trim(), MAX_MATCHED_CHARS),
                });
                break;
            }
        }
    }
    claims
}

/// A row that counts as a success record.
fn counted(row: &Value) -> bool {
    row.get("success").and_then(Value::as_bool) == Some(true)
        && row.get("usage_only").and_then(Value::as_bool) != Some(true)
}

fn tool_is(row: &Value, tools: &[&str]) -> bool {
    row.get("tool_name")
        .and_then(Value::as_str)
        .is_some_and(|name| tools.contains(&name))
}

/// A row that backs a "mail sent" claim: a [`MAIL_SENT_EVIDENCE_TOOLS`]
/// row with the boolean `sent == true`.
fn mail_sent(row: &Value) -> bool {
    tool_is(row, MAIL_SENT_EVIDENCE_TOOLS) && row.get("sent").and_then(Value::as_bool) == Some(true)
}

/// Check workspace claims against rows ALREADY scoped to this employee and
/// session (see [`crate::audit::read_session_tool_calls`]). Returns the
/// unbacked claims only; non-workspace claims are ignored.
pub fn verify_workspace_claims(claims: &[ActionClaim], rows: &[Value]) -> Vec<UnbackedClaim> {
    let counted_rows: Vec<&Value> = rows.iter().filter(|r| counted(r)).collect();
    claims
        .iter()
        .filter(|c| c.claim_type.is_workspace())
        .filter_map(|claim| {
            let backed = if claim.claim_type == ClaimType::MailSent {
                counted_rows.iter().any(|r| mail_sent(r))
            } else {
                let tools = claim.claim_type.evidence_tools();
                counted_rows.iter().any(|r| tool_is(r, tools))
            };
            if backed {
                return None;
            }
            let draft_only = claim.claim_type == ClaimType::MailSent
                && counted_rows.iter().any(|r| tool_is(r, MAIL_DRAFT_TOOLS));
            let (kind, reason) = if draft_only {
                (
                    UnbackedKind::DraftOnly,
                    format!(
                        "Agent claimed '{}' but this session only has a draft or pending-confirmation mail; nothing was sent",
                        claim.matched_text
                    ),
                )
            } else {
                let expected = if claim.claim_type == ClaimType::MailSent {
                    format!("{} (sent=true)", MAIL_SENT_EVIDENCE_TOOLS.join(" / "))
                } else {
                    claim.claim_type.evidence_tools().join(" / ")
                };
                (
                    UnbackedKind::NoRecord,
                    format!(
                        "Agent claimed '{}' but no successful {} row was found in this session",
                        claim.matched_text, expected
                    ),
                )
            };
            Some(UnbackedClaim { claim: claim.clone(), kind, reason })
        })
        .collect()
}

/// Check already-extracted `claims` against this employee's rows in
/// `session_id` (this `turn_id`, or the last
/// [`SESSION_EVIDENCE_WINDOW_MINUTES`] minutes before `now`). Blocking file
/// I/O; the log lock wait is bounded by `lock_wait`. No session id (or an
/// empty one), an empty agent id or no workspace claim ⇒ `Ok` and nothing.
/// An unavailable lock or unreadable log ⇒ `Err`: the caller must treat it
/// as "unknown" (no notice, no audit), never as "unbacked".
pub fn verify_claims_in_session(
    home_dir: &Path,
    agent_id: &str,
    session_id: Option<&str>,
    turn_id: Option<&str>,
    claims: &[ActionClaim],
    now: chrono::DateTime<chrono::Utc>,
    lock_wait: Duration,
) -> std::io::Result<Vec<UnbackedClaim>> {
    let Some(session_id) = session_id.filter(|s| !s.is_empty()) else {
        return Ok(Vec::new());
    };
    if agent_id.is_empty() || !claims.iter().any(|c| c.claim_type.is_workspace()) {
        return Ok(Vec::new());
    }
    let since = now - chrono::Duration::minutes(SESSION_EVIDENCE_WINDOW_MINUTES);
    let rows = crate::audit::read_session_tool_calls(home_dir, agent_id, session_id, turn_id, since, lock_wait)?;
    Ok(verify_workspace_claims(claims, &rows))
}

/// Full pipeline for one reply: [`extract_workspace_claims`] then
/// [`verify_claims_in_session`] with [`SESSION_EVIDENCE_LOCK_WAIT`]. No
/// session id (or an empty one) ⇒ nothing is evaluated and nothing is
/// returned. Blocking; `Err` means the evidence could not be read.
pub fn detect_unbacked_workspace_claims(
    home_dir: &Path,
    agent_id: &str,
    session_id: Option<&str>,
    turn_id: Option<&str>,
    output: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> std::io::Result<Vec<UnbackedClaim>> {
    if session_id.is_none_or(str::is_empty) || agent_id.is_empty() {
        return Ok(Vec::new());
    }
    let claims = extract_workspace_claims(output);
    if claims.is_empty() {
        return Ok(Vec::new());
    }
    verify_claims_in_session(home_dir, agent_id, session_id, turn_id, &claims, now, SESSION_EVIDENCE_LOCK_WAIT)
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
