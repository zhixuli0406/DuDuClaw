//! Query planning for the trigram FTS index and the short-term fallback.
//!
//! The trigram tokenizer cannot match anything shorter than three
//! characters, and most Chinese words are two (「請假」「特休」), so:
//!
//! - Latin terms of 3+ characters (`a-1234`, `overtime`) become quoted
//!   phrases; CJK runs of 3+ characters expand into all their trigrams,
//!   OR-ed, at most [`MAX_TRIGRAMS`] in total.
//! - Terms of exactly two characters go to a bounded `LIKE` scan instead.
//! - The two-character windows of longer CJK runs are kept as a fallback:
//!   when the trigram list finds too little, they join the `LIKE` scan
//!   (「特休天數」 still finds a passage that only says 「特休」).
//! - Single characters are ignored.
//!
//! Every term is emitted inside double quotes with embedded quotes doubled,
//! so FTS syntax in a query (`NEAR`, `*`, `-`, `:`, `OR`) is always literal
//! text and can never change the query's structure.

use crate::normalize::{Term, query_terms};

pub const MAX_TRIGRAMS: usize = 64;
/// Most two-character terms used for the `LIKE` fallback.
pub const MAX_LIKE_TERMS: usize = 8;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryPlan {
    /// FTS5 `MATCH` expression, `None` when no term qualifies.
    pub fts_match: Option<String>,
    /// Lower-cased two-character terms for the `LIKE` scan.
    pub like_terms: Vec<String>,
    /// Two-character windows of 3+ character CJK runs, used only when the
    /// FTS list is short.
    pub fallback_terms: Vec<String>,
}

impl QueryPlan {
    pub fn is_empty(&self) -> bool {
        self.fts_match.is_none() && self.like_terms.is_empty()
    }
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Build the plan for `query`.
pub fn plan_query(query: &str) -> QueryPlan {
    let mut phrases: Vec<String> = Vec::new();
    let mut like_terms: Vec<String> = Vec::new();
    let mut fallback_terms: Vec<String> = Vec::new();
    let push_unique = |v: &mut Vec<String>, s: String| {
        if !v.contains(&s) {
            v.push(s);
        }
    };
    for term in query_terms(query) {
        let n = term.char_len();
        match term {
            Term::Latin(t) if n >= 3 => push_unique(&mut phrases, quote(&t)),
            Term::Cjk(t) if n >= 3 => {
                let chars: Vec<char> = t.chars().collect();
                for w in chars.windows(2) {
                    if fallback_terms.len() < MAX_LIKE_TERMS {
                        push_unique(&mut fallback_terms, w.iter().collect());
                    }
                }
                for w in chars.windows(3) {
                    if phrases.len() >= MAX_TRIGRAMS {
                        break;
                    }
                    push_unique(&mut phrases, quote(&w.iter().collect::<String>()));
                }
            }
            Term::Latin(t) | Term::Cjk(t) if n == 2 && like_terms.len() < MAX_LIKE_TERMS => {
                push_unique(&mut like_terms, t);
            }
            _ => {}
        }
        if phrases.len() >= MAX_TRIGRAMS {
            phrases.truncate(MAX_TRIGRAMS);
        }
    }
    fallback_terms.retain(|t| !like_terms.contains(t));
    QueryPlan {
        fts_match: (!phrases.is_empty()).then(|| phrases.join(" OR ")),
        like_terms,
        fallback_terms,
    }
}

/// Escape `%`, `_` and `\` for `LIKE … ESCAPE '\'`.
pub fn like_pattern(term: &str) -> String {
    let mut out = String::with_capacity(term.len() + 2);
    out.push('%');
    for c in term.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_runs_become_trigrams_and_short_terms_fall_back() {
        let p = plan_query("新進員工請假 HR");
        assert_eq!(
            p.fts_match.as_deref(),
            Some("\"新進員\" OR \"進員工\" OR \"員工請\" OR \"工請假\"")
        );
        assert_eq!(p.like_terms, vec!["hr".to_string()]);
        assert_eq!(
            p.fallback_terms[..3],
            ["新進".to_string(), "進員".to_string(), "員工".to_string()]
        );
        let p = plan_query("請假");
        assert!(p.fts_match.is_none());
        assert_eq!(p.like_terms, vec!["請假".to_string()]);
    }

    #[test]
    fn fts_syntax_is_quoted() {
        let p = plan_query("abc\" OR NEAR(x y) * col:val -drop");
        let m = p.fts_match.unwrap();
        // Every token is quoted; the only bare words are the joining ORs.
        for part in m.split(" OR ") {
            assert!(part.starts_with('"') && part.ends_with('"'), "{part}");
        }
        assert!(m.contains("\"abc\""));
        assert!(m.contains("\"near\""));
        assert!(m.contains("\"col\""));
    }

    #[test]
    fn product_codes_stay_whole() {
        let p = plan_query("A-1234 的價格");
        assert!(p.fts_match.unwrap().contains("\"a-1234\""));
        assert_eq!(p.like_terms, vec!["價格".to_string()]);
    }

    #[test]
    fn trigram_cap() {
        let p = plan_query(&"字詞".repeat(100));
        let n = p.fts_match.unwrap().split(" OR ").count();
        assert!(n <= MAX_TRIGRAMS);
    }

    #[test]
    fn like_escapes_wildcards() {
        assert_eq!(like_pattern("5%_\\"), "%5\\%\\_\\\\%");
    }

    #[test]
    fn one_char_terms_are_ignored() {
        assert!(plan_query("的 x ？").is_empty());
    }
}
