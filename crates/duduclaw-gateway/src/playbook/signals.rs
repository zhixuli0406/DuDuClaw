//! §1.3 trigger-signal vocabulary, turn-signal assembly, and matching.
//!
//! Namespaced tokens (`mistake:` / `source_kind:` / `error:` / `failure:` /
//! `channel:` / `tool:` / `kw:` / `*`). Matching is OR: an entry is
//! "signal-matched" when ANY of its `signals_match` tokens is present in the
//! turn's assembled [`TurnSignals`] set.

use std::collections::BTreeSet;

use unicode_normalization::UnicodeNormalization;

/// A turn's assembled signal set. Namespaced tokens, all lowercase, `:`
/// delimited (or the literal `*` wildcard, though a turn itself never emits
/// `*` — only entries declare it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnSignals {
    tokens: BTreeSet<String>,
}

impl TurnSignals {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn contains(&self, token: &str) -> bool {
        self.tokens.contains(token)
    }

    pub fn tokens(&self) -> impl Iterator<Item = &str> {
        self.tokens.iter().map(String::as_str)
    }

    fn insert(&mut self, token: impl Into<String>) {
        self.tokens.insert(token.into());
    }

    /// `mistake:<factual|behavioral|capability|safety|hallucination>`.
    pub fn with_mistake_category(mut self, cat: &str) -> Self {
        self.insert(format!("mistake:{cat}"));
        self
    }

    /// `source_kind:<decision_gap|task_failure|unattributed>` — an empty
    /// input (unattributed/legacy `MistakeEntry.source_kind`) maps to the
    /// literal `unattributed` value per §1.3's value domain.
    pub fn with_source_kind(mut self, kind: &str) -> Self {
        let v = if kind.is_empty() { "unattributed" } else { kind };
        self.insert(format!("source_kind:{v}"));
        self
    }

    /// `error:<significant|critical>` — Negligible/Moderate are not part of
    /// the §1.3 value domain (they aren't error signals worth triggering on).
    pub fn with_error_category(mut self, cat: &str) -> Self {
        if cat == "significant" || cat == "critical" {
            self.insert(format!("error:{cat}"));
        }
        self
    }

    /// `failure:<FailureReason::as_str()>`.
    pub fn with_failure_reason(mut self, reason: &str) -> Self {
        self.insert(format!("failure:{reason}"));
        self
    }

    /// `channel:<telegram|line|discord|...>`.
    pub fn with_channel(mut self, channel: &str) -> Self {
        self.insert(format!("channel:{channel}"));
        self
    }

    /// `tool:<mcp tool name>` — exact token equality, never a substring match.
    pub fn with_tool(mut self, tool: &str) -> Self {
        self.insert(format!("tool:{tool}"));
        self
    }

    /// Derive `kw:` tokens from a user message using the SAME normalization
    /// as entry-authoring (§1.3), uncapped: the write side caps an entry's
    /// own `kw:` tokens at 3 to bound entry size, but a turn's signal set
    /// should include every qualifying token from the message so a stored
    /// entry keyword (however normalized) can be matched by plain set
    /// membership — no raw `contains` anywhere (security convention #2).
    pub fn with_keywords_from_message(mut self, message: &str) -> Self {
        for kw in extract_keywords(message, usize::MAX) {
            self.insert(format!("kw:{kw}"));
        }
        self
    }
}

/// Extract up to `max` normalized keyword tokens from `message`: NFKC →
/// lowercase → ASCII whitespace-split tokens with `chars().count() > 3` →
/// plus 2-grams from contiguous runs of CJK (Han) characters. CJK-safe
/// throughout (`char`s, never bytes). Used both at entry-authoring time
/// (capped at 3 per entry, §1.3) and at turn-signal assembly time (uncapped,
/// via [`TurnSignals::with_keywords_from_message`]).
pub fn extract_keywords(message: &str, max: usize) -> Vec<String> {
    let normalized: String = message.nfkc().flat_map(|c| c.to_lowercase()).collect();
    let mut out: Vec<String> = Vec::new();

    for tok in normalized.split_whitespace() {
        if out.len() >= max {
            return out;
        }
        let cleaned: String = tok.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        if cleaned.chars().count() > 3 && !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }

    let mut runs: Vec<Vec<char>> = Vec::new();
    let mut run: Vec<char> = Vec::new();
    for c in normalized.chars() {
        if is_han(c) {
            run.push(c);
        } else if !run.is_empty() {
            runs.push(std::mem::take(&mut run));
        }
    }
    if !run.is_empty() {
        runs.push(run);
    }
    for run in runs {
        if run.len() < 2 {
            continue;
        }
        for w in run.windows(2) {
            if out.len() >= max {
                return out;
            }
            let gram: String = w.iter().collect();
            if !out.contains(&gram) {
                out.push(gram);
            }
        }
    }
    out
}

fn is_han(c: char) -> bool {
    matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF)
}

/// §1.3 match semantics: OR, with `*` as an always-true wildcard.
pub fn matches(entry_signals: &[String], turn: &TurnSignals) -> bool {
    entry_signals.iter().any(|s| s == "*" || turn.contains(s))
}

/// Does the entry carry at least one discriminating (non-wildcard) trigger
/// token? A wildcard-only signal set fires on every turn, which makes the
/// entry unfalsifiable as a shadow risk predictor: armed on every
/// observation, its hit rate converges to the baseline itself, so the
/// Wilson CI straddles the baseline forever — unpromotable AND unretirable.
/// The arming scan (`select::collect_armed_shadow`) uses this to keep such
/// entries out of the trial family entirely.
pub fn has_discriminating_signal(entry_signals: &[String]) -> bool {
    entry_signals.iter().any(|s| s != "*")
}

/// [`matches`] restricted to discriminating tokens — the wildcard is
/// ignored, so an entry only "arms" on a turn its specific trigger actually
/// fired on. This keeps a mixed signal set (`["*", "mistake:capability"]`)
/// falsifiable: as an ACTIVE entry it still wildcard-matches for injection
/// ranking ([`matches`]), but as a shadow candidate it is only scored on the
/// turns its non-wildcard token selected.
pub fn matches_ignoring_wildcard(entry_signals: &[String], turn: &TurnSignals) -> bool {
    entry_signals.iter().any(|s| s != "*" && turn.contains(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_always_matches() {
        let turn = TurnSignals::new();
        assert!(matches(&["*".to_string()], &turn));
    }

    #[test]
    fn empty_signals_never_match() {
        let turn = TurnSignals::new().with_channel("telegram");
        assert!(!matches(&[], &turn));
    }

    #[test]
    fn discriminating_signal_detection_ignores_wildcard() {
        assert!(!has_discriminating_signal(&[]));
        assert!(!has_discriminating_signal(&["*".to_string()]));
        assert!(has_discriminating_signal(&["*".to_string(), "kw:refund".to_string()]));
        assert!(has_discriminating_signal(&["mistake:capability".to_string()]));
    }

    #[test]
    fn matches_ignoring_wildcard_requires_a_real_token_hit() {
        let turn = TurnSignals::new().with_mistake_category("capability");
        // Wildcard alone never arms.
        assert!(!matches_ignoring_wildcard(&["*".to_string()], &turn));
        // Mixed set arms only through its discriminating token…
        let mixed = vec!["*".to_string(), "mistake:capability".to_string()];
        assert!(matches_ignoring_wildcard(&mixed, &turn));
        // …and not on a turn where that token is absent (plain `matches`
        // would say yes via the wildcard — that asymmetry is the point).
        let other = TurnSignals::new().with_channel("telegram");
        assert!(!matches_ignoring_wildcard(&mixed, &other));
        assert!(matches(&mixed, &other));
    }

    #[test]
    fn namespaced_token_matches_exact_set_membership() {
        let turn = TurnSignals::new()
            .with_mistake_category("capability")
            .with_channel("telegram");
        assert!(matches(&["mistake:capability".to_string()], &turn));
        assert!(!matches(&["mistake:factual".to_string()], &turn));
        assert!(matches(&["channel:telegram".to_string()], &turn));
    }

    #[test]
    fn source_kind_empty_maps_to_unattributed() {
        let turn = TurnSignals::new().with_source_kind("");
        assert!(turn.contains("source_kind:unattributed"));
    }

    #[test]
    fn error_category_only_significant_and_critical_are_signals() {
        let turn = TurnSignals::new()
            .with_error_category("negligible")
            .with_error_category("moderate")
            .with_error_category("significant");
        assert!(!turn.contains("error:negligible"));
        assert!(!turn.contains("error:moderate"));
        assert!(turn.contains("error:significant"));
    }

    #[test]
    fn keyword_extraction_ascii_whitespace_and_length_gate() {
        let kws = extract_keywords("the cat sat on a mat quickly", 10);
        // "the"(3) "cat"(3) "sat"(3) "mat"(3) len<=3 excluded; "quickly"(7) kept.
        assert!(kws.contains(&"quickly".to_string()));
        assert!(!kws.contains(&"the".to_string()));
    }

    #[test]
    fn keyword_extraction_cjk_2grams() {
        let kws = extract_keywords("請幫我查詢訂單狀態", 10);
        assert!(kws.iter().any(|k| k.chars().count() == 2));
        assert!(kws.contains(&"查詢".to_string()) || kws.contains(&"訂單".to_string()));
    }

    #[test]
    fn keyword_extraction_respects_max() {
        let kws = extract_keywords("alpha beta gamma delta epsilon", 2);
        assert_eq!(kws.len(), 2);
    }

    #[test]
    fn turn_keywords_are_uncapped_unlike_entry_authoring_cap() {
        let turn = TurnSignals::new().with_keywords_from_message(
            "alpha beta gamma delta epsilon zeta eta theta",
        );
        // Every qualifying ASCII word (len>3) should be present, well beyond
        // the entry-authoring cap of 3.
        assert!(turn.contains("kw:alpha"));
        assert!(turn.contains("kw:theta"));
    }

    #[test]
    fn matching_never_uses_unanchored_contains() {
        // "hi" must not match inside a token like "this" — matching is exact
        // set membership on namespaced tokens, not substring search.
        let turn = TurnSignals::new().with_keywords_from_message("this thing");
        assert!(!matches(&["kw:hi".to_string()], &turn));
    }
}
