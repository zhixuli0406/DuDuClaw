//! Visible-text predicates and the Unicode character classes shared by the
//! other `llm_contract` modules.
//!
//! Mirrors Cloudflare security-audit-skill (MIT) `VISIBLE_CONTENT`,
//! `PATH_FORBIDDEN_CHARACTER` and `isVisibleText`. Rust `&str` is always a
//! sequence of Unicode scalar values, so the skill's lone-surrogate check
//! (`hasValidUnicodeScalarValues`) holds by construction.
//!
//! The property tables below are hand-written from the Unicode 16.0 UCD
//! (`DerivedGeneralCategory.txt`, `DerivedCoreProperties.txt`) so that no
//! extra crate is needed. `White_Space` uses [`char::is_whitespace`], which is
//! defined as that exact property.

/// General_Category = Cc (control).
pub(crate) fn is_cc(c: char) -> bool {
    c.is_control()
}

/// General_Category = Cf (format).
pub(crate) fn is_cf(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

/// General_Category = Zl (line separator) or Zp (paragraph separator).
pub(crate) fn is_zl_or_zp(c: char) -> bool {
    matches!(c, '\u{2028}' | '\u{2029}')
}

/// Default_Ignorable_Code_Point.
pub(crate) fn is_default_ignorable(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD
            | 0x034F
            | 0x061C
            | 0x115F..=0x1160
            | 0x17B4..=0x17B5
            | 0x180B..=0x180F
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0x3164
            | 0xFE00..=0xFE0F
            | 0xFEFF
            | 0xFFA0
            | 0xFFF0..=0xFFF8
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0000..=0xE0FFF
    )
}

/// `[\p{Cc}\p{Cf}\p{Zl}\p{Zp}\p{Default_Ignorable_Code_Point}]` — characters
/// that may never appear in a path or a canonical reference (NUL included,
/// it is Cc).
pub(crate) fn is_forbidden_identifier_char(c: char) -> bool {
    is_cc(c) || is_cf(c) || is_zl_or_zp(c) || is_default_ignorable(c)
}

/// `[^\p{White_Space}\p{Cc}\p{Cf}\p{Default_Ignorable_Code_Point}]`.
pub(crate) fn is_visible_char(c: char) -> bool {
    !(c.is_whitespace() || is_cc(c) || is_cf(c) || is_default_ignorable(c))
}

/// True when `s` contains at least one code point that is not White_Space,
/// Cc, Cf or Default_Ignorable — i.e. something a human would actually see.
pub fn has_visible_content(s: &str) -> bool {
    s.chars().any(is_visible_char)
}

/// Bounded, trimmed, visible text: non-empty, at most `max_chars` code points,
/// no leading/trailing whitespace, and [`has_visible_content`].
pub fn is_visible_text(s: &str, max_chars: usize) -> bool {
    !s.is_empty() && s.chars().count() <= max_chars && s.trim() == s && has_visible_content(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_content_matches_reference_corpus() {
        assert!(has_visible_content("Valid prose."));
        assert!(has_visible_content("caf\u{e9}"));
        assert!(has_visible_content("客戶"));
        assert!(!has_visible_content(""));
        assert!(!has_visible_content(" \t\r\n"));
        assert!(!has_visible_content("\u{200b}"));
        assert!(!has_visible_content("\u{34f}"));
        assert!(!has_visible_content("\u{fe0f}"));
        assert!(!has_visible_content("\u{3000}\u{feff}\u{0}"));
        assert!(!has_visible_content("\u{e0041}"));
    }

    #[test]
    fn visible_text_bounds() {
        assert!(is_visible_text("abc", 3));
        assert!(!is_visible_text("abcd", 3));
        assert!(
            is_visible_text("客戶資料", 4),
            "counted in code points, not bytes"
        );
        assert!(!is_visible_text(" abc", 10));
        assert!(!is_visible_text("abc\n", 10));
        assert!(!is_visible_text("", 10));
        assert!(!is_visible_text("\u{200b}", 10));
        assert!(is_visible_text("a b", 10));
    }

    #[test]
    fn forbidden_identifier_classes() {
        for c in [
            '\0',
            '\u{1f}',
            '\u{7f}',
            '\u{85}',
            '\u{ad}',
            '\u{200b}',
            '\u{200e}',
            '\u{202e}',
            '\u{2028}',
            '\u{2029}',
            '\u{2066}',
            '\u{feff}',
            '\u{34f}',
            '\u{fe0f}',
            '\u{e0001}',
            '\u{180e}',
            '\u{3164}',
        ] {
            assert!(is_forbidden_identifier_char(c), "{:?} must be forbidden", c);
        }
        for c in ['a', 'Z', '0', '-', ' ', '客', '\u{e9}', '\u{3000}'] {
            assert!(!is_forbidden_identifier_char(c), "{:?} must be allowed", c);
        }
    }
}
