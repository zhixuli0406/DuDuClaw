//! The check result: rule levels, reasons, the merge, and the short-lived
//! record that lets a person confirm (or send for approval) exactly what was
//! checked without running the AI review again.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::FileKey;

/// The merged outcome, ordered `Pass < Warn < Block`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Verdict {
    Pass,
    Warn,
    Block,
}

impl Verdict {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Block => "block",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "pass" => Some(Self::Pass),
            "warn" => Some(Self::Warn),
            "block" => Some(Self::Block),
            _ => None,
        }
    }
}

/// What one fixed rule decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RuleLevel {
    None,
    /// At least a warning.
    Flag,
    /// A block.
    Escalate,
    /// The content is invalid: it cannot be applied at all, only fixed.
    Reject,
}

/// Reason categories shown to people (plain words in the dashboard). The
/// full set is listed even where only the AI review produces a category.
#[allow(dead_code)]
pub(crate) mod category {
    pub const PROMPT_INJECTION: &str = "prompt_injection";
    pub const SECRET_REQUEST: &str = "secret_request";
    pub const LOOSENED_REDLINE: &str = "loosened_redline";
    pub const CONTRACT_CONFLICT: &str = "contract_conflict";
    pub const ROLE_DRIFT: &str = "role_drift";
    pub const HIDDEN_CONTENT: &str = "hidden_content";
    pub const SECRET_IN_TEXT: &str = "secret_in_text";
    pub const SIZE: &str = "size";
    pub const FORMAT: &str = "format";
    pub const ANALYSIS_UNAVAILABLE: &str = "analysis_unavailable";
    /// The file had been changed outside the dashboard before this save.
    pub const OUTSIDE_CHANGE: &str = "outside_change";
    /// A skill asks the employee to run something dangerous.
    pub const RISKY_ACTION: &str = "risky_action";
    /// A skill already held suspicious lines this change did not touch.
    pub const EXISTING_RISK: &str = "existing_risk";
}

/// One reason the check gave.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Reason {
    pub category: String,
    /// `low | medium | high`.
    pub severity: String,
    pub message: String,
    /// At most 200 characters of the offending text.
    pub evidence: String,
}

impl Reason {
    pub(crate) fn new(category: &str, severity: &str, message: impl Into<String>, evidence: &str) -> Self {
        Self {
            category: category.to_string(),
            severity: severity.to_string(),
            message: message.into(),
            evidence: duduclaw_core::truncate_chars(evidence, 200),
        }
    }
}

/// A rule finding: its level plus the reason shown.
#[derive(Debug, Clone)]
pub(crate) struct RuleFinding {
    pub level: RuleLevel,
    pub reason: Reason,
}

/// Highest rule level among `findings`.
pub(crate) fn rule_level(findings: &[RuleFinding]) -> RuleLevel {
    findings.iter().map(|f| f.level).max().unwrap_or(RuleLevel::None)
}

/// The verdict the rules alone give (`Reject` is handled before this).
pub(crate) fn rules_verdict(level: RuleLevel) -> Verdict {
    match level {
        RuleLevel::None => Verdict::Pass,
        RuleLevel::Flag => Verdict::Warn,
        RuleLevel::Escalate | RuleLevel::Reject => Verdict::Block,
    }
}

/// How the AI review went.
#[derive(Debug, Clone)]
pub(crate) enum AiOutcome {
    /// Not run for this file or source.
    NotApplicable,
    /// `partial`: the reviewer only saw part of the change (the diff or the
    /// text was cut to fit), so the result counts as at least a warning.
    Done { verdict: Verdict, summary: String, reasons: Vec<Reason>, partial: bool },
    /// Timed out, unparseable, over quota, no utility model…
    Failed(String),
}

/// `final = max(rules, ai, failed ? on_failure : pass)`. The AI can only
/// make it stricter; a failed review is never a pass.
pub(crate) fn merge(rule: RuleLevel, ai: &AiOutcome, on_failure: Verdict) -> Verdict {
    let r = rules_verdict(rule);
    match ai {
        AiOutcome::NotApplicable => r,
        AiOutcome::Done { verdict, partial, .. } => {
            let v = r.max(*verdict);
            if *partial { v.max(Verdict::Warn) } else { v }
        }
        AiOutcome::Failed(_) => r.max(on_failure.max(Verdict::Warn)),
    }
}

pub(crate) fn basis(ai: &AiOutcome) -> &'static str {
    match ai {
        AiOutcome::NotApplicable => "rules",
        AiOutcome::Done { .. } => "rules+ai",
        AiOutcome::Failed(_) => "rules+ai_unavailable",
    }
}

/// The reason added when the review did not complete.
pub(crate) fn unavailable_reason() -> Reason {
    Reason::new(
        category::ANALYSIS_UNAVAILABLE,
        "medium",
        "這次沒能完成自動檢查，請自己確認內容",
        "",
    )
}

/// The reason added when the reviewer only saw part of the change.
pub(crate) fn partial_reason() -> Reason {
    Reason::new(
        category::ANALYSIS_UNAVAILABLE,
        "medium",
        "內容太長，自動檢查只看了一部分，請自己確認其餘內容",
        "",
    )
}

/// Whether the automatic review did not cover the whole change (failed or
/// partial). A non-admin cannot confirm such a result themselves.
pub(crate) fn incomplete(reasons: &[Reason]) -> bool {
    reasons.iter().any(|r| r.category == category::ANALYSIS_UNAVAILABLE)
}

/// Category names, deduplicated, in first-seen order (for audit and history).
pub(crate) fn categories(reasons: &[Reason]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in reasons {
        if !out.contains(&r.category) {
            out.push(r.category.clone());
        }
    }
    out
}

// ── The confirm record ──────────────────────────────────────────────────────

/// How long a check result can be confirmed or sent for approval.
pub(crate) const ANALYSIS_TTL: Duration = Duration::from_secs(15 * 60);
/// At most this many check results are kept at once.
const MAX_RECORDS: usize = 1000;

/// What one check was about: a confirm or approval request must match every
/// field (same person, employee, file, starting version and content).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Analysis {
    pub user_id: String,
    pub agent_id: String,
    pub file: FileKey,
    pub base_hash: String,
    pub content_sha256: String,
    pub verdict: Verdict,
    pub basis: &'static str,
    pub summary: String,
    pub reasons: Vec<Reason>,
    pub source: &'static str,
}

struct Stored {
    at: Instant,
    analysis: Analysis,
}

fn store() -> &'static Mutex<HashMap<String, Stored>> {
    static STORE: OnceLock<Mutex<HashMap<String, Stored>>> = OnceLock::new();
    STORE.get_or_init(Default::default)
}

/// Keep `a` for [`ANALYSIS_TTL`]; returns its id. In memory only: after a
/// restart the change has to be checked again.
pub(crate) fn remember(a: Analysis) -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let mut map = match store().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    map.retain(|_, s| s.at.elapsed() < ANALYSIS_TTL);
    // Bounded: past the cap the oldest records go first.
    while map.len() >= MAX_RECORDS {
        let Some(oldest) = map.iter().min_by_key(|(_, s)| s.at).map(|(k, _)| k.clone()) else { break };
        map.remove(&oldest);
    }
    map.insert(id.clone(), Stored { at: Instant::now(), analysis: a });
    id
}

/// The record for `id` when it is still valid and matches `expected` in
/// every binding field (verdict and reasons come from the record).
pub(crate) fn recall(id: &str, user_id: &str, agent_id: &str, file: &FileKey, base_hash: &str, content_sha256: &str) -> Result<Analysis, String> {
    let map = match store().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let s = map.get(id).ok_or("這次檢查的結果已過期或不存在，請重新檢查")?;
    if s.at.elapsed() >= ANALYSIS_TTL {
        return Err("這次檢查的結果已過期（超過 15 分鐘），請重新檢查".into());
    }
    let a = &s.analysis;
    if a.user_id != user_id || a.agent_id != agent_id || a.file != *file || a.base_hash != base_hash || a.content_sha256 != content_sha256 {
        return Err("內容或檔案版本和檢查時不同，請重新檢查".into());
    }
    Ok(a.clone())
}

/// Drop a record once it was used.
pub(crate) fn forget(id: &str) {
    if let Ok(mut map) = store().lock() {
        map.remove(id);
    }
}

/// The `review` object of the save envelope.
pub(crate) fn review_json(analysis_id: Option<&str>, a: &Analysis, is_admin: bool) -> Value {
    // An incomplete review (failed, over quota, or partial) cannot be
    // confirmed by a non-admin: they can only send it to an admin.
    let can_confirm = match a.verdict {
        Verdict::Pass => false,
        Verdict::Warn => is_admin || !incomplete(&a.reasons),
        Verdict::Block => is_admin,
    };
    json!({
        "analysis_id": analysis_id,
        "verdict": a.verdict.as_str(),
        "basis": a.basis,
        "summary": a.summary,
        "reasons": a.reasons,
        "can_confirm": can_confirm,
        "can_request_approval": !is_admin && a.verdict != Verdict::Pass,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_only_makes_it_stricter_and_failure_is_never_a_pass() {
        let done = |v| AiOutcome::Done { verdict: v, summary: String::new(), reasons: vec![], partial: false };
        assert_eq!(merge(RuleLevel::Escalate, &done(Verdict::Pass), Verdict::Warn), Verdict::Block);
        assert_eq!(merge(RuleLevel::None, &done(Verdict::Block), Verdict::Warn), Verdict::Block);
        assert_eq!(merge(RuleLevel::None, &done(Verdict::Pass), Verdict::Warn), Verdict::Pass);
        let failed = AiOutcome::Failed("timeout".into());
        assert_eq!(merge(RuleLevel::None, &failed, Verdict::Warn), Verdict::Warn);
        assert_eq!(merge(RuleLevel::None, &failed, Verdict::Block), Verdict::Block);
        // Even a misconfigured "pass" on failure cannot make it a pass.
        assert_eq!(merge(RuleLevel::None, &failed, Verdict::Pass), Verdict::Warn);
        assert_eq!(merge(RuleLevel::Flag, &AiOutcome::NotApplicable, Verdict::Warn), Verdict::Warn);
        let partial = AiOutcome::Done { verdict: Verdict::Pass, summary: String::new(), reasons: vec![], partial: true };
        assert_eq!(merge(RuleLevel::None, &partial, Verdict::Warn), Verdict::Warn, "a partial review is at least a warning");
    }

    fn analysis() -> Analysis {
        Analysis {
            user_id: "u".into(),
            agent_id: "a1".into(),
            file: FileKey::Soul,
            base_hash: "b".into(),
            content_sha256: "c".into(),
            verdict: Verdict::Warn,
            basis: "rules+ai",
            summary: String::new(),
            reasons: vec![],
            source: "dashboard",
        }
    }

    #[test]
    fn confirm_record_binds_every_field() {
        let id = remember(analysis());
        assert!(recall(&id, "u", "a1", &FileKey::Soul, "b", "c").is_ok());
        assert!(recall(&id, "other", "a1", &FileKey::Soul, "b", "c").is_err(), "another user");
        assert!(recall(&id, "u", "a2", &FileKey::Soul, "b", "c").is_err(), "another employee");
        assert!(recall(&id, "u", "a1", &FileKey::Identity, "b", "c").is_err(), "another file");
        assert!(recall(&id, "u", "a1", &FileKey::Soul, "b2", "c").is_err(), "another base");
        assert!(recall(&id, "u", "a1", &FileKey::Soul, "b", "c2").is_err(), "other content");
        forget(&id);
        assert!(recall(&id, "u", "a1", &FileKey::Soul, "b", "c").is_err(), "used up");
        assert!(recall("missing", "u", "a1", &FileKey::Soul, "b", "c").is_err());
    }

    #[test]
    fn review_json_permissions() {
        let mut a = analysis();
        let v = review_json(Some("x"), &a, false);
        assert_eq!(v["can_confirm"], true);
        assert_eq!(v["can_request_approval"], true);
        a.reasons = vec![unavailable_reason()];
        assert_eq!(review_json(None, &a, false)["can_confirm"], false, "incomplete: non-admins only send it");
        assert_eq!(review_json(None, &a, true)["can_confirm"], true);
        a.verdict = Verdict::Block;
        assert_eq!(review_json(None, &a, false)["can_confirm"], false);
        assert_eq!(review_json(None, &a, true)["can_confirm"], true);
        assert_eq!(review_json(None, &a, true)["can_request_approval"], false);
    }
}
