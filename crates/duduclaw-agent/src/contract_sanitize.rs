//! Sanitize CONTRACT.toml rules that came from outside (an LLM-generated
//! team, a third-party expert pack) before they take effect.
//!
//! `must_not` is not only prompt guidance: [`crate::contract::validate_response`]
//! matches every rule as a case-insensitive SUBSTRING (or a glob when it
//! carries `*` `?` `[`) against each channel reply and blocks the reply on a
//! hit. So an external rule like `*` would block every reply, and `退款`
//! every reply that mentions refunds. Rules are therefore normalized:
//!
//! - glob metacharacters are removed (outside text never authors a pattern);
//! - rules shorter than [`MIN_MUST_NOT_CHARS`] are dropped (keyword-like);
//! - rules found verbatim in what the employee is meant to SAY (its persona
//!   minus the red-line sections: escalation wording, example answers) are
//!   dropped, because they would block the employee's own handoff replies;
//! - length, count and duplicates are capped.
//!
//! Every dropped rule is returned with a reason so callers can report it.

use std::collections::BTreeSet;

use crate::contract::Boundaries;

/// Shortest `must_not` rule kept (chars).
pub const MIN_MUST_NOT_CHARS: usize = 10;
const MIN_MUST_ALWAYS_CHARS: usize = 4;
const MAX_RULE_CHARS: usize = 160;
const MAX_RULES: usize = 8;

/// Section headings treated as red-line sections (excluded from the
/// "spoken" text a `must_not` rule must not match).
const RED_LINE_HEADINGS: [&str; 7] = [
    "紅線", "絕不", "禁止", "red line", "must not", "boundar", "never",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    TooShort,
    MatchesOwnWording,
    Duplicate,
    OverLimit,
}

impl DropReason {
    pub fn zh(self) -> &'static str {
        match self {
            DropReason::TooShort => "太短，會擋掉正常回覆",
            DropReason::MatchesOwnWording => "和員工自己該說的話相同，會擋掉自己的回覆",
            DropReason::Duplicate => "重複",
            DropReason::OverLimit => "超過條數上限",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Sanitized {
    pub boundaries: Boundaries,
    /// `(original rule, reason)` for every rule that was dropped.
    pub dropped: Vec<(String, DropReason)>,
}

fn normalize_rule(raw: &str) -> String {
    let flat: String = raw
        .chars()
        .map(|c| if c.is_whitespace() || c.is_control() { ' ' } else { c })
        .filter(|c| !matches!(c, '*' | '?' | '[' | ']'))
        .collect();
    let collapsed = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    duduclaw_core::truncate_chars(collapsed.trim(), MAX_RULE_CHARS)
}

/// The persona text an employee is expected to say: everything except the
/// `#`-headed sections whose heading names red lines.
pub fn spoken_text(soul: &str) -> String {
    let mut out = String::new();
    let mut in_red_lines = false;
    for line in soul.lines() {
        let t = line.trim_start();
        if t.starts_with('#') {
            let heading = t.to_lowercase();
            in_red_lines = RED_LINE_HEADINGS.iter().any(|k| heading.contains(k));
            continue;
        }
        if !in_red_lines {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn sanitize_list(
    rules: &[String],
    min_chars: usize,
    spoken_lower: Option<&str>,
    dropped: &mut Vec<(String, DropReason)>,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut kept = Vec::new();
    for raw in rules {
        let rule = normalize_rule(raw);
        let reason = if rule.chars().count() < min_chars {
            Some(DropReason::TooShort)
        } else if spoken_lower.is_some_and(|s| s.contains(&rule.to_lowercase())) {
            Some(DropReason::MatchesOwnWording)
        } else if !seen.insert(rule.clone()) {
            Some(DropReason::Duplicate)
        } else if kept.len() >= MAX_RULES {
            Some(DropReason::OverLimit)
        } else {
            None
        };
        match reason {
            Some(r) => dropped.push((raw.clone(), r)),
            None => kept.push(rule),
        }
    }
    kept
}

/// Sanitize external rules. `soul` (the employee's persona) enables the
/// own-wording check for `must_not`; pass `None` when there is none.
pub fn sanitize_boundaries(b: &Boundaries, soul: Option<&str>) -> Sanitized {
    let spoken = soul.map(|s| spoken_text(s).to_lowercase());
    let mut dropped = Vec::new();
    let must_not = sanitize_list(&b.must_not, MIN_MUST_NOT_CHARS, spoken.as_deref(), &mut dropped);
    let must_always = sanitize_list(&b.must_always, MIN_MUST_ALWAYS_CHARS, None, &mut dropped);
    Sanitized {
        boundaries: Boundaries {
            must_not,
            must_always,
            max_tool_calls_per_turn: b.max_tool_calls_per_turn,
        },
        dropped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(must_not: &[&str], must_always: &[&str]) -> Boundaries {
        Boundaries {
            must_not: must_not.iter().map(|s| s.to_string()).collect(),
            must_always: must_always.iter().map(|s| s.to_string()).collect(),
            max_tool_calls_per_turn: 0,
        }
    }

    #[test]
    fn wildcards_keywords_and_duplicates_are_dropped_with_reasons() {
        let out = sanitize_boundaries(
            &b(
                &["*", "退款", "*保證*獲利*", "替客戶決定要不要退費或承諾退費金額", "替客戶決定要不要退費或承諾退費金額"],
                &["要", "涉及金額一律轉交真人確認"],
            ),
            None,
        );
        assert_eq!(
            out.boundaries.must_not,
            vec!["替客戶決定要不要退費或承諾退費金額".to_string()]
        );
        assert_eq!(out.boundaries.must_always, vec!["涉及金額一律轉交真人確認".to_string()]);
        let reasons: Vec<DropReason> = out.dropped.iter().map(|(_, r)| *r).collect();
        assert_eq!(
            reasons,
            vec![
                DropReason::TooShort,
                DropReason::TooShort,
                DropReason::TooShort,
                DropReason::Duplicate,
                DropReason::TooShort
            ]
        );
    }

    #[test]
    fn rules_matching_the_employees_own_handoff_wording_are_dropped() {
        let soul = "# 小安\n\n## 升級真人\n遇到金額時我會說：這部分需要轉給店長確認後回覆您。\n\n## 紅線\n- 替客戶決定要不要退費或承諾退費金額\n";
        let out = sanitize_boundaries(
            &b(&["這部分需要轉給店長確認後回覆您", "替客戶決定要不要退費或承諾退費金額"], &[]),
            Some(soul),
        );
        // The red-line section itself does not count as spoken text.
        assert_eq!(
            out.boundaries.must_not,
            vec!["替客戶決定要不要退費或承諾退費金額".to_string()]
        );
        assert_eq!(out.dropped[0].1, DropReason::MatchesOwnWording);
    }

    #[test]
    fn surviving_rules_do_not_block_an_ordinary_refusal() {
        let out = sanitize_boundaries(&b(&["*", "退款", "替客戶決定要不要退費或承諾退費金額"], &[]), None);
        let contract = crate::contract::Contract {
            boundaries: out.boundaries,
        };
        let v = crate::contract::validate_response(&contract, "退款的部分我先轉給店長，他會回覆您。");
        assert!(v.passed, "{:?}", v.violations);
    }
}
