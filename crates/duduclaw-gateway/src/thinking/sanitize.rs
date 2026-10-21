//! Make thinking text safe to show: mask, check, truncate.
//!
//! Never restores anything. Channel replies go through
//! `channel_reply::restore_for_channel`, which puts the original values back
//! for the end user; thinking text must not, so this module never calls it
//! (or `RedactionPipeline::restore`). In order:
//!
//! 1. `<REDACT:CAT:hash>` tokens become `〔已遮蔽·CAT〕` (no vault lookup).
//! 2. With redaction enabled, the operator's rule engine runs over the text
//!    and every span becomes `〔已遮蔽·category〕` (no vault write, no NER).
//! 3. Credential-shaped words (vendor key prefixes, bearer tokens, JWTs,
//!    secrets in URLs) are replaced whole — not `first4***last4`, because
//!    successive snapshots of a growing block would otherwise reveal
//!    different characters of the same key. CJK text glued to a key does not
//!    hide it: ASCII runs are scanned on their own.
//! 4. Control characters and stray `<think>` tags are removed, runs of blank
//!    lines collapsed.
//! 5. The outbound guardrail + CONTRACT checks
//!    (`channel_reply::check_outbound_with`) run on the result; a refusal is
//!    returned to the caller, which withholds the text.
//! 6. Truncation by chars (CJK safe).

use std::sync::Arc;

use duduclaw_redaction::RuleEngine;

use crate::channel_reply::{OutboundBlock, OutboundRules, OutboundVerdict, check_outbound_with};

/// Placeholder for a credential-shaped word.
pub const SECRET_PLACEHOLDER: &str = "〔已遮蔽·secret〕";
/// Max chars of the one-line progress (`line`).
pub const LINE_CHARS: usize = 80;

/// What the masking needs to know about the turn.
pub struct SanitizeCtx<'a> {
    /// The answering employee's guardrail + CONTRACT rules (loaded once per
    /// turn by the sink).
    pub rules: Arc<OutboundRules>,
    /// The operator's rule engine — `Some` only when redaction is enabled.
    pub engine: Option<&'a RuleEngine>,
}

/// Result of [`sanitize_for_display`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sanitized {
    /// `line` is the last non-empty line of the whole masked text (before
    /// truncation), so it keeps following a long block's latest progress.
    Text {
        text: String,
        truncated: bool,
        line: Option<String>,
    },
    /// The outbound checks refused the text; show none of it.
    Blocked(OutboundBlock),
}

/// Steps 1–6 (see the module docs). `cap_chars` is the truncation budget.
pub fn sanitize_for_display(text: &str, ctx: &SanitizeCtx<'_>, cap_chars: usize) -> Sanitized {
    let masked = mask_text(text, ctx.engine);
    match check_outbound_with(&masked, &ctx.rules) {
        OutboundVerdict::Blocked(block) => Sanitized::Blocked(block),
        OutboundVerdict::Allow => truncate(masked, cap_chars),
        OutboundVerdict::Redacted(t) => truncate(t, cap_chars),
    }
}

/// Steps 1–4 only (pure).
pub fn mask_text(text: &str, engine: Option<&RuleEngine>) -> String {
    let step1 = duduclaw_redaction::token::mask_tokens_for_display(text);
    let step2 = match engine {
        Some(engine) => mask_rule_spans(&step1, engine),
        None => step1,
    };
    let step3 = mask_secret_words(&step2);
    clean_text(&step3)
}

fn truncate(text: String, cap_chars: usize) -> Sanitized {
    let line = last_line(&text);
    let truncated = text.chars().count() > cap_chars;
    let text = if truncated {
        duduclaw_core::truncate_chars(&text, cap_chars).to_string()
    } else {
        text
    };
    Sanitized::Text {
        text,
        truncated,
        line,
    }
}

/// Step 2: replace every rule-engine span with its category placeholder.
fn mask_rule_spans(text: &str, engine: &RuleEngine) -> String {
    let source = duduclaw_redaction::Source::ToolResult {
        tool_name: "thinking".to_string(),
    };
    let spans = engine.apply(text, &source);
    if spans.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for m in &spans {
        // Spans are non-overlapping, sorted, and on char boundaries (regex
        // match offsets). Anything else is a bug upstream: withhold the
        // whole text rather than guess which part was meant.
        let (Some(before), Some(_)) = (
            text.get(cursor..m.span.start),
            text.get(m.span.start..m.span.end),
        ) else {
            return duduclaw_redaction::token::display_placeholder("text");
        };
        out.push_str(before);
        out.push_str(&duduclaw_redaction::token::display_placeholder(
            m.rule.category(),
        ));
        cursor = m.span.end;
    }
    out.push_str(text.get(cursor..).unwrap_or_default());
    out
}

/// Same delimiter set as `duduclaw_core::secret_redact` uses between words.
fn is_word_delim(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '"' | '\'' | ',' | ';'
        )
}

/// Split into alternating word / delimiter runs (`true` = delimiter run).
fn word_runs(s: &str) -> Vec<(bool, &str)> {
    let mut runs = Vec::new();
    let mut start = 0usize;
    let mut current: Option<bool> = None;
    for (i, c) in s.char_indices() {
        let d = is_word_delim(c);
        match current {
            Some(prev) if prev == d => {}
            Some(prev) => {
                runs.push((prev, &s[start..i]));
                start = i;
                current = Some(d);
            }
            None => current = Some(d),
        }
    }
    if let Some(prev) = current {
        runs.push((prev, &s[start..]));
    }
    runs
}

/// Step 3 for one ASCII run: words the shared secret redactor would touch
/// are replaced whole. If the two word lists ever fail to line up, the whole
/// run is replaced (fail closed).
fn mask_secret_run(run: &str) -> String {
    let redacted = duduclaw_core::secret_redact::redact_secrets(run);
    if redacted.as_ref() == run {
        return run.to_string();
    }
    let before = word_runs(run);
    let after = word_runs(&redacted);
    if before.len() != after.len() {
        return SECRET_PLACEHOLDER.to_string();
    }
    before
        .iter()
        .zip(after.iter())
        .map(|((delim, a), (_, b))| {
            if *delim || a == b {
                (*a).to_string()
            } else {
                SECRET_PLACEHOLDER.to_string()
            }
        })
        .collect()
}

/// Step 3: scan each maximal ASCII run separately so a key written right
/// after CJK text (`金鑰是sk-ant-…`) is still seen as its own word.
pub fn mask_secret_words(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut start = 0usize;
    let mut ascii: Option<bool> = None;
    let flush = |out: &mut String, seg: &str, is_ascii: bool| {
        if is_ascii {
            out.push_str(&mask_secret_run(seg));
        } else {
            out.push_str(seg);
        }
    };
    for (i, c) in text.char_indices() {
        let a = c.is_ascii();
        match ascii {
            Some(prev) if prev == a => {}
            Some(prev) => {
                flush(&mut out, &text[start..i], prev);
                start = i;
                ascii = Some(a);
            }
            None => ascii = Some(a),
        }
    }
    if let Some(prev) = ascii {
        flush(&mut out, &text[start..], prev);
    }
    out
}

/// Step 4: drop control characters (keep `\n` / `\t`), stray think tags,
/// and collapse three or more consecutive newlines to two.
fn clean_text(text: &str) -> String {
    let no_tags = text
        .replace(duduclaw_core::think_tags::THINK_OPEN, "")
        .replace(duduclaw_core::think_tags::THINK_CLOSE, "");
    let mut out = String::with_capacity(no_tags.len());
    let mut newlines = 0usize;
    for c in no_tags.chars() {
        if c == '\r' || (c.is_control() && c != '\n' && c != '\t') {
            continue;
        }
        if c == '\n' {
            newlines += 1;
            if newlines > 2 {
                continue;
            }
        } else if !c.is_whitespace() {
            newlines = 0;
        }
        out.push(c);
    }
    out.trim().to_string()
}

/// The last non-empty line, trimmed and capped at [`LINE_CHARS`].
pub fn last_line(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| duduclaw_core::truncate_chars(l, LINE_CHARS).to_string())
}

/// Longest tail held back when the cut would split an ASCII word.
pub const MAX_WORD_HOLD_BACK_CHARS: usize = 512;

/// Split a growing block into the part that is safe to show now and a tail
/// held back until the block ends: everything after the last whitespace,
/// but at most `max_tail_chars` (so whitespace-free CJK text still flows).
/// A secret still being streamed sits in the tail, where a half-written key
/// cannot slip past the shape checks. If the `max_tail_chars` cut would land
/// inside an ASCII word (a long key or JWT with no whitespace), the whole
/// word is held back, up to [`MAX_WORD_HOLD_BACK_CHARS`].
pub fn split_hold_back(text: &str, max_tail_chars: usize) -> (&str, &str) {
    let ws_cut = text
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let total = text.chars().count();
    let cap_cut = if total > max_tail_chars {
        text.char_indices()
            .nth(total - max_tail_chars)
            .map(|(i, _)| i)
            .unwrap_or(text.len())
    } else {
        0
    };
    let cut = if cap_cut > ws_cut {
        word_start_before(
            text,
            cap_cut,
            MAX_WORD_HOLD_BACK_CHARS.saturating_sub(max_tail_chars),
        )
    } else {
        ws_cut
    };
    text.split_at(cut)
}

/// Move `cut` back to the start of the ASCII word it splits, by at most
/// `max_back` chars. A cut between two non-ASCII chars (CJK) stays.
fn word_start_before(text: &str, cut: usize, max_back: usize) -> usize {
    let is_word = |c: char| c.is_ascii() && !c.is_whitespace();
    let splits_word = text[cut..].chars().next().is_some_and(is_word)
        && text[..cut].chars().next_back().is_some_and(is_word);
    if !splits_word {
        return cut;
    }
    let mut start = cut;
    for (moved, (i, c)) in text[..cut].char_indices().rev().enumerate() {
        if !is_word(c) || moved >= max_back {
            break;
        }
        start = i;
    }
    start
}

#[cfg(test)]
#[path = "sanitize_tests.rs"]
mod tests;
