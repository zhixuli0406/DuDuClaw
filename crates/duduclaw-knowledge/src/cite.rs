//! Citation markers in a reply, the excerpt shown for each source, and the
//! plain-text source footer for messaging apps.
//!
//! A reply cites passage `n` with `[n]` (1–99). Full-width forms
//! (`【1】`, `［1］`, `[１]`) are normalised first. When the turn offered
//! passages, a marker whose number was never offered is removed (and
//! counted as invalid); a turn that offered nothing keeps its text exactly
//! as written. `[1](…)` is a Markdown link, not a citation, and is left
//! alone.

use serde::{Deserialize, Serialize};

/// Highest citation number.
pub const MAX_CITATION: u32 = 99;
/// Most sources listed in a footer.
pub const MAX_FOOTER_SOURCES: usize = 5;
/// Excerpt length in the footer (characters, before the ellipsis).
pub const EXCERPT_CHARS: usize = 40;

/// One cited (or offered) passage, as shown to the person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationRef {
    pub n: u32,
    pub chunk_id: i64,
    pub source_id: String,
    pub title: String,
    pub locator: String,
    pub excerpt: String,
}

/// What parsing a reply found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedCitations {
    /// The reply with invalid markers removed and full-width markers
    /// normalised.
    pub text: String,
    /// Valid numbers in order of first use, without repeats.
    pub cited: Vec<u32>,
    /// Numbers that were not offered this turn (removed from `text`).
    pub invalid: Vec<u32>,
}

fn fullwidth_digit(c: char) -> Option<char> {
    match c {
        '０'..='９' => char::from_u32('0' as u32 + (c as u32 - '０' as u32)),
        _ => None,
    }
}

/// Rewrite `【n】`, `［n］` and full-width digits inside brackets to `[n]`.
pub fn normalize_markers(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let close = match c {
            '【' => Some('】'),
            '［' => Some('］'),
            '[' => Some(']'),
            _ => None,
        };
        if let Some(close) = close {
            let mut j = i + 1;
            let mut digits = String::new();
            while j < chars.len() && digits.chars().count() < 3 {
                let d = chars[j];
                if d.is_ascii_digit() {
                    digits.push(d);
                } else if let Some(a) = fullwidth_digit(d) {
                    digits.push(a);
                } else {
                    break;
                }
                j += 1;
            }
            let closes =
                j < chars.len() && (chars[j] == close || (close != ']' && chars[j] == ']'));
            if !digits.is_empty() && digits.len() <= 2 && closes {
                out.push('[');
                out.push_str(&digits);
                out.push(']');
                i = j + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Find citation markers. `offered` = the numbers this turn offered; empty
/// ⇒ the text is returned unchanged (no knowledge was used).
pub fn parse_citations(text: &str, offered: &[u32]) -> ParsedCitations {
    if offered.is_empty() {
        return ParsedCitations {
            text: text.to_string(),
            ..Default::default()
        };
    }
    let norm = normalize_markers(text);
    let chars: Vec<char> = norm.chars().collect();
    let mut out = String::with_capacity(norm.len());
    let mut cited = Vec::new();
    let mut invalid = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            let mut j = i + 1;
            while j < chars.len() && j - i <= 3 && chars[j].is_ascii_digit() {
                j += 1;
            }
            let len = j - i - 1;
            let is_link = chars.get(j + 1) == Some(&'(');
            if (1..=2).contains(&len) && chars.get(j) == Some(&']') && !is_link {
                let n: u32 = chars[i + 1..j]
                    .iter()
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0);
                if n >= 1 && offered.contains(&n) {
                    if !cited.contains(&n) {
                        cited.push(n);
                    }
                    out.push_str(&format!("[{n}]"));
                } else if !invalid.contains(&n) {
                    invalid.push(n);
                }
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    ParsedCitations {
        text: tidy_spaces(&out),
        cited,
        invalid,
    }
}

/// Collapse the double spaces a removed marker can leave behind.
fn tidy_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c == ' ' {
            if prev_space {
                continue;
            }
            prev_space = true;
        } else {
            prev_space = false;
        }
        out.push(c);
    }
    out
}

/// Character trigrams of the normalised, lower-cased query.
fn trigrams(s: &str) -> Vec<String> {
    let chars: Vec<char> = crate::normalize::match_form(s)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if chars.len() < 3 {
        return if chars.is_empty() {
            Vec::new()
        } else {
            vec![chars.iter().collect()]
        };
    }
    chars.windows(3).map(|w| w.iter().collect()).collect()
}

/// The sentence of `passage` that shares the most query trigrams, cut to
/// [`EXCERPT_CHARS`] characters with an ellipsis. Ties keep the earliest
/// sentence; with no overlap the first sentence is used.
pub fn excerpt(passage: &str, query: &str) -> String {
    let grams = trigrams(query);
    let mut best: Option<(usize, &str)> = None;
    for sentence in passage
        .split(['。', '！', '？', '；', '\n', '!', '?', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let hay = crate::normalize::match_form(sentence);
        let score = grams.iter().filter(|g| hay.contains(g.as_str())).count();
        if best.is_none_or(|(b, _)| score > b) {
            best = Some((score, sentence));
        }
    }
    let Some((_, sentence)) = best else {
        return String::new();
    };
    let flat = inline(sentence, usize::MAX);
    if flat.chars().count() > EXCERPT_CHARS {
        format!("{}…", duduclaw_core::truncate_chars(&flat, EXCERPT_CHARS))
    } else {
        flat
    }
}

/// One line of metadata: control characters become spaces, runs collapse,
/// square brackets become 〔〕 (a title cannot forge a marker).
pub fn inline(s: &str, max: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| match c {
            c if c.is_control() => ' ',
            '[' => '〔',
            ']' => '〕',
            c => c,
        })
        .collect();
    let joined = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if max == usize::MAX {
        joined
    } else {
        duduclaw_core::truncate_chars(&joined, max)
    }
}

/// Largest footer (characters) per messaging channel, leaving the platform's
/// message limit to the answer itself (Discord 2 000, Telegram 4 096,
/// LINE 5 000 per message).
pub fn footer_cap(channel: &str) -> usize {
    match channel {
        "discord" => 600,
        "telegram" | "line" | "slack" | "whatsapp" => 1_200,
        _ => 1_000,
    }
}

/// The plain-text source list for messaging apps: at most
/// [`MAX_FOOTER_SOURCES`] cited sources, within `cap` characters. `None`
/// when nothing was cited.
pub fn render_footer(cited: &[CitationRef], cap: usize) -> Option<String> {
    if cited.is_empty() {
        return None;
    }
    let mut out = String::from("來源：");
    let mut shown = 0usize;
    for c in cited.iter().take(MAX_FOOTER_SOURCES) {
        let mut entry = format!(
            "\n[{}] {} {}",
            c.n,
            inline(&c.title, 60),
            inline(&c.locator, 40)
        );
        let ex = inline(&c.excerpt, EXCERPT_CHARS + 1);
        if !ex.is_empty() {
            entry.push_str(&format!("\n「{ex}」"));
        }
        if out.chars().count() + entry.chars().count() > cap {
            break;
        }
        out.push_str(&entry);
        shown += 1;
    }
    if shown == 0 {
        return None;
    }
    let rest = cited.len() - shown;
    if rest > 0 {
        let more = format!("\n（另有 {rest} 筆來源）");
        if out.chars().count() + more.chars().count() <= cap {
            out.push_str(&more);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cref(n: u32, title: &str, excerpt: &str) -> CitationRef {
        CitationRef {
            n,
            chunk_id: n as i64,
            source_id: format!("src_{n}"),
            title: title.into(),
            locator: "第 3 頁".into(),
            excerpt: excerpt.into(),
        }
    }

    #[test]
    fn normalises_full_width_markers() {
        assert_eq!(normalize_markers("特休七日【1】。"), "特休七日[1]。");
        assert_eq!(normalize_markers("見［２］與[１２]"), "見[2]與[12]");
        assert_eq!(normalize_markers("【123】不是"), "【123】不是");
        assert_eq!(normalize_markers("【附註】"), "【附註】");
    }

    #[test]
    fn valid_invalid_and_untouched_markers() {
        let p = parse_citations("滿一年給七日[1]，另見[9]與【2】。", &[1, 2]);
        assert_eq!(p.text, "滿一年給七日[1]，另見與[2]。");
        assert_eq!(p.cited, vec![1, 2]);
        assert_eq!(p.invalid, vec![9]);
        // Repeats count once; [0] is never valid.
        let p = parse_citations("A [1] B [1] C [0]", &[1]);
        assert_eq!(p.cited, vec![1]);
        assert_eq!(p.invalid, vec![0]);
        assert_eq!(p.text, "A [1] B [1] C ");
        // No passages offered: text untouched, nothing parsed.
        let p = parse_citations("footnote [3]", &[]);
        assert_eq!(p.text, "footnote [3]");
        assert!(p.cited.is_empty() && p.invalid.is_empty());
    }

    #[test]
    fn malformed_markers_and_links_are_left_alone() {
        let p = parse_citations("[1](https://x) [123] [a] [ 1] [1 [", &[1]);
        assert_eq!(p.text, "[1](https://x) [123] [a] [ 1] [1 [");
        assert!(p.cited.is_empty());
        let p = parse_citations("結尾[1]", &[1]);
        assert_eq!(p.cited, vec![1]);
        let p = parse_citations("[01]", &[1]);
        assert_eq!(p.text, "[1]");
    }

    #[test]
    fn excerpt_picks_the_overlapping_sentence() {
        let passage = "第一章 總則。特休假依年資計算，滿一年者給予七日特休假。其他規定另訂。";
        let e = excerpt(passage, "特休假怎麼計算");
        assert!(e.starts_with("特休假依年資計算"), "{e}");
        let long = format!("{}。", "很長的句子".repeat(20));
        let e = excerpt(&long, "無關");
        assert!(e.ends_with('…'));
        assert_eq!(e.chars().count(), EXCERPT_CHARS + 1);
        assert_eq!(excerpt("", "q"), "");
    }

    #[test]
    fn footer_lists_cited_sources_within_cap() {
        let list: Vec<CitationRef> = (1..=7)
            .map(|n| cref(n, "員工手冊.pdf", "特休假依年資計算，滿一年者給予七日…"))
            .collect();
        let f = render_footer(&list, 10_000).unwrap();
        assert!(f.starts_with("來源：\n[1] 員工手冊.pdf 第 3 頁\n「特休假依年資計算"));
        assert_eq!(f.matches("\n[").count(), MAX_FOOTER_SOURCES);
        assert!(f.ends_with("（另有 2 筆來源）"));
        for ch in ["discord", "line", "telegram", "slack", "teams"] {
            let f = render_footer(&list, footer_cap(ch)).unwrap();
            assert!(f.chars().count() <= footer_cap(ch), "{ch}");
        }
        let tiny = render_footer(&list, 40).unwrap_or_default();
        assert!(tiny.chars().count() <= 40);
        assert!(render_footer(&[], 1_000).is_none());
    }

    #[test]
    fn titles_cannot_forge_markers_or_lines() {
        let f = render_footer(&[cref(1, "a]\n[2] fake [x", "")], 1_000).unwrap();
        assert_eq!(f, "來源：\n[1] a〕 〔2〕 fake 〔x 第 3 頁");
    }
}
