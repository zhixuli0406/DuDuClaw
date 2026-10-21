//! `@`-mention matching for calling an AI employee by name.
//!
//! Pure text logic, no registry and no authorization: the caller supplies the
//! employees a mention may name ([`MentionTarget`]) and a predicate telling
//! which of them the sender may call. Routing and refusal policy live in the
//! gateway (`trigger_route`).
//!
//! Rules (replacing the old unanchored `contains` match, coding convention 2):
//! - A mention starts with `@` (or the full-width `＠`) at the start of the
//!   text or after a character that cannot end an e-mail local part, so
//!   `ann@example.com` is never a mention.
//! - After the `@` the longest employee name (trigger without its `@`,
//!   display name, or employee id) that matches case-insensitively wins, so
//!   `@財務小幫手` beats `@財務` when both exist.
//! - A name must end at a boundary: end of text, or any character unless both
//!   the name's last character and the next one are ASCII word characters.
//!   A CJK name may be followed directly by text (`@財務小幫手幫我算帳`); an
//!   ASCII name may not run into more ASCII (`@ann` does not match `@anna`).
//! - The earliest mention of an employee the sender may call wins. A mention
//!   of an employee the sender may not call is reported only when no callable
//!   employee is mentioned.
//! - `@` alone, or `@` plus the start of one or more callable names, is a
//!   request for the list; a partial name never routes silently.

/// An employee a mention may name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionTarget {
    /// Employee id (`[agent] name`).
    pub id: String,
    /// Names that call this employee, without a leading `@`.
    pub names: Vec<String>,
}

impl MentionTarget {
    /// Build a target from an employee's id, display name and trigger. Empty
    /// names are dropped, a leading `@`/`＠` is removed from each, and
    /// duplicates are removed.
    pub fn new(id: &str, display_name: &str, trigger: &str) -> Self {
        let mut names: Vec<String> = Vec::new();
        for raw in [trigger, display_name, id] {
            let name = strip_at(raw.trim()).trim();
            if !name.is_empty() && !names.iter().any(|n| eq_ci(n, name)) {
                names.push(name.to_string());
            }
        }
        Self {
            id: id.to_string(),
            names,
        }
    }
}

/// One full mention found in the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionHit {
    /// The employee id.
    pub id: String,
    /// Byte offset of the `@`.
    pub start: usize,
    /// Byte offset just after the matched name.
    pub end: usize,
}

/// What the text asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MentionScan {
    /// No usable mention.
    None,
    /// The earliest mention of an employee the sender may call.
    Hit(MentionHit),
    /// Only employees the sender may not call are mentioned; the first one.
    Forbidden(MentionHit),
    /// `@` alone or a partial / ambiguous name: the callable employees it
    /// could mean, in target order. `token` is what followed the `@`.
    Partial {
        token: String,
        candidates: Vec<String>,
    },
}

/// Longest token taken after a bare `@` when looking for a partial name.
const MAX_PARTIAL_CHARS: usize = 32;

/// Scan `text` for mentions of `targets`. `may_call(id)` says whether the
/// sender may call that employee.
pub fn scan_mentions(
    text: &str,
    targets: &[MentionTarget],
    may_call: &dyn Fn(&str) -> bool,
) -> MentionScan {
    let mut forbidden: Option<MentionHit> = None;
    let mut partial: Option<MentionScan> = None;
    for at in mention_starts(text) {
        let after = at + at_len(text, at);
        let rest = &text[after..];
        match longest_names_at(rest, targets) {
            Some((len, ids)) => {
                let end = after + len;
                let callable: Vec<&String> = ids.iter().filter(|id| may_call(id)).collect();
                match callable.as_slice() {
                    [one] => {
                        return MentionScan::Hit(MentionHit {
                            id: (*one).clone(),
                            start: at,
                            end,
                        });
                    }
                    [] => {
                        if forbidden.is_none() {
                            forbidden = Some(MentionHit {
                                id: ids[0].clone(),
                                start: at,
                                end,
                            });
                        }
                    }
                    many => {
                        // Several callable employees share this exact name:
                        // never guess, offer the choice.
                        if partial.is_none() {
                            partial = Some(MentionScan::Partial {
                                token: rest[..len].to_string(),
                                candidates: many.iter().map(|s| (*s).clone()).collect(),
                            });
                        }
                    }
                }
            }
            None => {
                if partial.is_none() {
                    let token = partial_token(rest);
                    // A lone `@` asks for the list only when nothing but
                    // whitespace follows it (`@`, `找誰 @`), so prose such as
                    // `meet @ 5pm` is never a list request.
                    if token.is_empty() && !rest.trim().is_empty() {
                        continue;
                    }
                    let candidates: Vec<String> = targets
                        .iter()
                        .filter(|t| may_call(&t.id))
                        .filter(|t| {
                            token.is_empty() || t.names.iter().any(|n| starts_with_ci(n, token))
                        })
                        .map(|t| t.id.clone())
                        .collect();
                    if !candidates.is_empty() {
                        partial = Some(MentionScan::Partial {
                            token: token.to_string(),
                            candidates,
                        });
                    }
                }
            }
        }
    }
    if let Some(hit) = forbidden {
        return MentionScan::Forbidden(hit);
    }
    partial.unwrap_or(MentionScan::None)
}

/// The text with the mention at `hit` removed, trimmed of whitespace and of
/// the separators people type after a name (`,` `:` `，` `：` …). Empty means
/// the message only called the employee and carries no task.
pub fn task_text_without(text: &str, hit: &MentionHit) -> String {
    let joined = format!("{}{}", &text[..hit.start], &text[hit.end..]);
    joined
        .trim_matches(|c: char| c.is_whitespace() || is_separator(c))
        .to_string()
}

fn is_separator(c: char) -> bool {
    matches!(
        c,
        ',' | ':'
            | ';'
            | '.'
            | '!'
            | '?'
            | '，'
            | '：'
            | '；'
            | '。'
            | '！'
            | '？'
            | '、'
            | '~'
            | '～'
    )
}

fn strip_at(s: &str) -> &str {
    s.strip_prefix('@')
        .or_else(|| s.strip_prefix('＠'))
        .unwrap_or(s)
}

fn is_at(c: char) -> bool {
    c == '@' || c == '＠'
}

fn at_len(text: &str, at: usize) -> usize {
    text[at..].chars().next().map(char::len_utf8).unwrap_or(1)
}

/// Byte offsets of every `@` that may start a mention.
fn mention_starts(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut prev: Option<char> = None;
    for (i, c) in text.char_indices() {
        if is_at(c) && prev.is_none_or(|p| !is_email_char(p)) {
            out.push(i);
        }
        prev = Some(c);
    }
    out
}

/// Characters that can end an e-mail local part (or a handle glued to `@`).
fn is_email_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-' | '@')
}

fn is_ascii_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Length in bytes of `rest`'s prefix that equals `name` case-insensitively,
/// when that prefix ends at a valid boundary.
fn match_name_at(rest: &str, name: &str) -> Option<usize> {
    let mut rest_chars = rest.char_indices();
    let mut last_name_char = None;
    for n in name.chars() {
        let (_, r) = rest_chars.next()?;
        if !chars_eq_ci(r, n) {
            return None;
        }
        last_name_char = Some(n);
    }
    let end = rest_chars.next().map(|(i, _)| i).unwrap_or(rest.len());
    let next = rest[end..].chars().next();
    match (last_name_char, next) {
        (Some(l), Some(c)) if is_ascii_word(l) && is_ascii_word(c) => None,
        (None, _) => None,
        _ => Some(end),
    }
}

/// The longest name matching at the start of `rest`, with every employee id
/// that has a name of that length matching there.
fn longest_names_at(rest: &str, targets: &[MentionTarget]) -> Option<(usize, Vec<String>)> {
    let mut best: Option<(usize, Vec<String>)> = None;
    for t in targets {
        let longest = t.names.iter().filter_map(|n| match_name_at(rest, n)).max();
        let Some(len) = longest else { continue };
        match &mut best {
            Some((b, ids)) if *b == len => {
                if !ids.contains(&t.id) {
                    ids.push(t.id.clone());
                }
            }
            Some((b, _)) if *b > len => {}
            _ => best = Some((len, vec![t.id.clone()])),
        }
    }
    best
}

/// What followed a bare `@`, up to a space or punctuation, capped.
fn partial_token(rest: &str) -> &str {
    let mut end = 0;
    for (count, (i, c)) in rest.char_indices().enumerate() {
        if count >= MAX_PARTIAL_CHARS || c.is_whitespace() || is_token_break(c) {
            return &rest[..i];
        }
        end = i + c.len_utf8();
    }
    &rest[..end]
}

fn is_token_break(c: char) -> bool {
    (c.is_ascii_punctuation() && !matches!(c, '-' | '_' | '.'))
        || is_separator(c)
        || is_at(c)
        || matches!(c, '「' | '」' | '（' | '）' | '【' | '】')
}

fn chars_eq_ci(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

fn eq_ci(a: &str, b: &str) -> bool {
    a.chars().count() == b.chars().count()
        && a.chars().zip(b.chars()).all(|(x, y)| chars_eq_ci(x, y))
}

fn starts_with_ci(name: &str, prefix: &str) -> bool {
    let mut n = name.chars();
    prefix
        .chars()
        .all(|p| n.next().is_some_and(|c| chars_eq_ci(c, p)))
}

#[cfg(test)]
#[path = "trigger_match_tests.rs"]
mod tests;
