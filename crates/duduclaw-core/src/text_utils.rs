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
    fn emoji_surrogate_pair_respected() {
        // 🐾 is 4 bytes (F0 9F 90 BE); 1 char.
        assert_eq!(truncate_bytes("a🐾b", 2), "a");
        assert_eq!(truncate_bytes("a🐾b", 5), "a🐾");
    }
}
