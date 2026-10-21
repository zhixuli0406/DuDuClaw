//! Platform-native mentions of people. LINE (`message.mention.mentionees`)
//! and Telegram (`entities` of type `mention` / `text_mention`) mark where a
//! user was @-mentioned, as UTF-16 code-unit offsets. Those spans name a
//! person, never an AI employee, so they are blanked before `@` matching:
//! `@Alice` (a group member) never asks for the employee list or calls an
//! employee whose name starts the same way.

/// Byte range of the UTF-16 span `[offset, offset + length)` in `text`, or
/// `None` when it does not fall on character boundaries inside the text.
pub fn utf16_span_to_bytes(text: &str, offset: usize, length: usize) -> Option<(usize, usize)> {
    let end16 = offset.checked_add(length)?;
    let (mut units, mut start, mut end) = (0usize, None, None);
    for (i, c) in text.char_indices() {
        if units == offset {
            start = Some(i);
        }
        if units == end16 {
            end = Some(i);
        }
        units += c.len_utf16();
    }
    if units == offset {
        start = Some(text.len());
    }
    if units == end16 {
        end = Some(text.len());
    }
    match (start, end) {
        (Some(s), Some(e)) if s <= e => Some((s, e)),
        _ => None,
    }
}

/// `text` with every UTF-16 span replaced by spaces (one per character).
/// Spans that do not fit the text are ignored.
pub fn blank_utf16_spans(text: &str, spans: &[(usize, usize)]) -> String {
    let mut ranges: Vec<(usize, usize)> = spans
        .iter()
        .filter_map(|(o, l)| utf16_span_to_bytes(text, *o, *l))
        .collect();
    if ranges.is_empty() {
        return text.to_string();
    }
    ranges.sort_unstable();
    let mut out = String::with_capacity(text.len());
    for (i, c) in text.char_indices() {
        if ranges.iter().any(|(s, e)| i >= *s && i < *e) {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_before_the_mention_counts_as_two_units() {
        // "😀 @Alice hi": 😀 is one char, two UTF-16 units.
        let text = "😀 @Alice hi";
        let (s, e) = utf16_span_to_bytes(text, 3, 6).unwrap();
        assert_eq!(&text[s..e], "@Alice");
        assert_eq!(blank_utf16_spans(text, &[(3, 6)]), "😀        hi");
    }

    #[test]
    fn cjk_and_out_of_range_spans() {
        let text = "請 @小王 和 @財務小幫手 看";
        let (s, e) = utf16_span_to_bytes(text, 2, 3).unwrap();
        assert_eq!(&text[s..e], "@小王");
        assert_eq!(
            blank_utf16_spans(text, &[(2, 3)]),
            "請     和 @財務小幫手 看"
        );
        assert_eq!(blank_utf16_spans(text, &[(40, 3)]), text);
        // A span ending inside a surrogate pair is ignored.
        assert!(utf16_span_to_bytes("😀x", 1, 1).is_none());
    }
}
