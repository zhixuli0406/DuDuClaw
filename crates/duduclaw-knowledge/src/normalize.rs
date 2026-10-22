//! Text normalisation for indexing and querying.
//!
//! Two forms of the same text:
//!
//! - **Display** ([`clean_text`]): what people read. Characters are kept as
//!   written (full-width punctuation stays full-width); only structure is
//!   cleaned: every line break becomes `\n`, tabs become a space, control
//!   and zero-width characters are dropped, trailing spaces are trimmed per
//!   line and runs of blank lines collapse to one. Every offset is counted in
//!   Unicode scalar values of this form.
//! - **Matching** ([`match_form`]): NFKC (full-width ASCII → half-width,
//!   Kangxi radicals → ideographs, compatibility forms folded) and lower
//!   case. Stored next to the display text and the only form the index and
//!   the substring scan look at; never shown.
//!
//! Query side ([`query_terms`]): the matching form, then split into CJK runs
//! and Latin/digit runs (`A-1234`, `hr`).

use unicode_normalization::UnicodeNormalization;

/// Clean extracted document text for display (no NFKC).
pub fn clean_text(raw: &str) -> String {
    let unified = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(unified.len());
    let mut blank_run = 0usize;
    for line in unified.split('\n') {
        let cleaned: String = line
            .chars()
            .filter_map(|c| match c {
                '\t' => Some(' '),
                c if c.is_control() => None,
                // Zero-width characters carry no text and break matching.
                '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' => None,
                c => Some(c),
            })
            .collect();
        let trimmed = cleaned.trim_end();
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out.push_str(trimmed);
        out.push('\n');
    }
    // Leading / trailing blank lines carry nothing.
    out.trim_matches('\n').to_string()
}

/// The matching form of `text`: NFKC, then lower case.
pub fn match_form(text: &str) -> String {
    text.nfkc().collect::<String>().to_lowercase()
}

/// Whether `c` belongs to a CJK script run (Han, kana, Hangul, CJK
/// punctuation is excluded).
pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF      // Hiragana, Katakana
        | 0x3400..=0x4DBF    // CJK Ext A
        | 0x4E00..=0x9FFF    // CJK Unified
        | 0xAC00..=0xD7AF    // Hangul syllables
        | 0xF900..=0xFAFF    // CJK compatibility ideographs
        | 0x20000..=0x2FA1F  // Ext B..F + compatibility supplement
    )
}

/// One query term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    /// A run of CJK characters.
    Cjk(String),
    /// A run of letters, digits and the joiners `-` `_` `.` `/` inside it.
    Latin(String),
}

impl Term {
    pub fn text(&self) -> &str {
        match self {
            Term::Cjk(s) | Term::Latin(s) => s,
        }
    }

    pub fn char_len(&self) -> usize {
        self.text().chars().count()
    }
}

/// Single-character particles that split a CJK query run (「請假的規定」 →
/// 「請假」「規定」), so a particle the document does not contain cannot
/// break every trigram around it. Query side only.
const CJK_PARTICLES: &[char] = &[
    '的', '了', '嗎', '呢', '吧', '啊', '呀', '和', '與', '及', '或', '是', '在',
];

fn is_latin_part(c: char) -> bool {
    c.is_alphanumeric() && !is_cjk(c)
}

/// Split a query into terms after NFKC + lower-casing. Joiners are kept only
/// between two Latin characters (`a-1234` stays one term, `-x` does not).
pub fn query_terms(query: &str) -> Vec<Term> {
    let norm = match_form(query);
    let chars: Vec<char> = norm.chars().collect();
    let mut terms = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if is_cjk(c) && !CJK_PARTICLES.contains(&c) {
            let start = i;
            while i < chars.len() && is_cjk(chars[i]) && !CJK_PARTICLES.contains(&chars[i]) {
                i += 1;
            }
            terms.push(Term::Cjk(chars[start..i].iter().collect()));
        } else if is_latin_part(c) {
            let start = i;
            while i < chars.len() {
                let ch = chars[i];
                let joiner = matches!(ch, '-' | '_' | '.' | '/')
                    && i + 1 < chars.len()
                    && is_latin_part(chars[i + 1]);
                if !is_latin_part(ch) && !joiner {
                    break;
                }
                i += 1;
            }
            terms.push(Term::Latin(chars[start..i].iter().collect()));
        } else {
            i += 1;
        }
    }
    terms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_keeps_characters_and_cleans_structure() {
        let t = clean_text("ＡＢＣ１２３，\r\n\r\n\r\n第二段\t結尾！   \u{200B}\n");
        assert_eq!(t, "ＡＢＣ１２３，\n\n第二段 結尾！");
    }

    #[test]
    fn matching_form_folds_width_case_and_radicals() {
        assert_eq!(match_form("ＡＢＣ１２３，⼀年"), "abc123,一年");
    }

    #[test]
    fn control_characters_are_removed() {
        assert_eq!(clean_text("a\u{0007}b\u{0000}c"), "abc");
    }

    #[test]
    fn query_terms_split_cjk_and_latin() {
        let terms = query_terms("新進員工 HR 的 A-1234 規定？ x");
        assert_eq!(
            terms,
            vec![
                Term::Cjk("新進員工".into()),
                Term::Latin("hr".into()),
                Term::Latin("a-1234".into()),
                Term::Cjk("規定".into()),
                Term::Latin("x".into()),
            ]
        );
    }

    #[test]
    fn full_width_query_folds() {
        assert_eq!(query_terms("ＨＲ"), vec![Term::Latin("hr".into())]);
    }
}
