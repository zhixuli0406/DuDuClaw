//! UTF-8-safe string truncation helpers.
//!
//! Rust string slicing (`s[..n]`) panics if `n` lands mid-char on a
//! multi-byte code point (any non-ASCII character). Channel adapters
//! that log snippets of user messages routinely hit this on CJK input,
//! panicking the tokio worker and breaking reply delivery. These
//! helpers truncate at a char boundary ≤ the requested byte budget.

/// Return a subslice containing at most `max_bytes` bytes of `s`, ending
/// on a valid UTF-8 char boundary. Never panics.
///
/// If `s.len() <= max_bytes`, returns `s` unchanged.
/// If `max_bytes` falls mid-char, walks back to the nearest boundary.
pub fn truncate_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Truncate `s` to at most `max_chars` characters (not bytes). Never panics.
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// One line of untrusted text made safe to place inside plain-text output
/// (tool results, logs, one-line summaries): C0 / C1 control characters
/// (newlines and tabs included), Unicode line / paragraph separators, bidi
/// controls and zero-width / byte-order marks are removed, then the result is
/// capped at `max_chars` characters with [`truncate_chars`]. Never slices by
/// byte index.
pub fn plain_line(s: &str, max_chars: usize) -> String {
    let cleaned: String = s.chars().filter(|c| !is_unsafe_inline_char(*c)).collect();
    truncate_chars(&cleaned, max_chars)
}

/// Characters [`plain_line`] removes.
pub fn is_unsafe_inline_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061C}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2069}'
                | '\u{FEFF}'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_shorter_than_limit_returns_input() {
        assert_eq!(truncate_bytes("hello", 10), "hello");
    }

    #[test]
    fn ascii_longer_than_limit_truncated() {
        assert_eq!(truncate_bytes("hello world", 5), "hello");
    }

    #[test]
    fn mid_cjk_char_walks_back_to_boundary() {
        // "學" is 3 bytes in UTF-8 (E5 AD B8).
        // "hi學" = 5 bytes; slicing at 4 would land mid-"學".
        assert_eq!(truncate_bytes("hi學", 4), "hi");
        assert_eq!(truncate_bytes("hi學", 5), "hi學");
        // Regression for channel_reply.rs:1241 panic message.
        let reply = "全部 14 個 Agent 建立完成";
        // 500 > reply.len(), so returns unchanged.
        assert_eq!(truncate_bytes(reply, 500), reply);
        // Force a mid-char cut to verify recovery.
        let long = "學".repeat(300); // 900 bytes
        let truncated = truncate_bytes(&long, 500);
        assert!(truncated.len() <= 500);
        // Result must still be valid UTF-8 (enforced by &str type).
        assert_eq!(truncated.chars().count() * 3, truncated.len());
    }

    #[test]
    fn zero_max_bytes_returns_empty() {
        assert_eq!(truncate_bytes("學生", 0), "");
    }

    #[test]
    fn truncate_chars_counts_codepoints_not_bytes() {
        assert_eq!(truncate_chars("學生好", 2), "學生");
        assert_eq!(truncate_chars("學生好", 10), "學生好");
    }

    #[test]
    fn plain_line_strips_controls_bidi_and_caps_by_chars() {
        assert_eq!(plain_line("a\nb\r\tc\u{1b}[31m", 64), "abc[31m");
        assert_eq!(plain_line("x\u{202E}y\u{2066}z\u{200B}\u{FEFF}\u{2028}", 64), "xyz");
        assert_eq!(plain_line("\u{85}員\u{9f}工", 64), "員工", "C1 controls");
        assert_eq!(plain_line(&"員".repeat(100), 64).chars().count(), 64);
        assert_eq!(plain_line("ok 名稱", 64), "ok 名稱");
    }

    #[test]
    fn emoji_surrogate_pair_respected() {
        // 🐾 is 4 bytes (F0 9F 90 BE); 1 char.
        assert_eq!(truncate_bytes("a🐾b", 2), "a");
        assert_eq!(truncate_bytes("a🐾b", 5), "a🐾");
    }
}
