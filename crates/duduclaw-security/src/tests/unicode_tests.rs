#[cfg(test)]
mod tests {
    use crate::unicode_normalizer::*;

    #[test]
    fn nfkc_normalizes_fullwidth_latin() {
        // Fullwidth 'Ａ' (U+FF21) should normalize to 'A' under NFKC
        let input = "\u{FF21}\u{FF22}\u{FF23}";
        let result = UnicodeNormalizer::normalize_nfkc(input);
        assert_eq!(result, "ABC");
    }

    #[test]
    fn nfc_preserves_fullwidth() {
        // NFC does not decompose compatibility characters
        let input = "\u{FF21}";
        let result = UnicodeNormalizer::normalize_nfc(input);
        assert_eq!(result, "\u{FF21}");
    }

    #[test]
    fn variation_selectors_stripped() {
        let input = "A\u{FE0F}B\u{FE01}C";
        let (cleaned, count) = UnicodeNormalizer::strip_invisible_chars(input);
        assert_eq!(cleaned, "ABC");
        assert_eq!(count, 2);
    }

    #[test]
    fn tag_characters_stripped() {
        let input = "hello\u{E0001}\u{E0041}\u{E007F}world";
        let (cleaned, count) = UnicodeNormalizer::strip_invisible_chars(input);
        assert_eq!(cleaned, "helloworld");
        assert_eq!(count, 3);
    }

    #[test]
    fn zero_width_chars_stripped() {
        let input = "he\u{200B}ll\u{200C}o\u{200D}\u{FEFF}";
        let (cleaned, count) = UnicodeNormalizer::strip_invisible_chars(input);
        assert_eq!(cleaned, "hello");
        assert_eq!(count, 4);
    }

    #[test]
    fn bidi_overrides_stripped() {
        let input = "abc\u{202A}def\u{202E}ghi\u{2066}jkl\u{2069}";
        let (cleaned, count) = UnicodeNormalizer::strip_invisible_chars(input);
        assert_eq!(cleaned, "abcdefghijkl");
        assert_eq!(count, 4);
    }

    #[test]
    fn ansi_escape_sequences_stripped() {
        let input = "\x1b[31mred text\x1b[0m normal \x1b[1;32mbold green\x1b[0m";
        let (cleaned, count) = UnicodeNormalizer::strip_ansi_escapes(input);
        assert_eq!(cleaned, "red text normal bold green");
        assert_eq!(count, 4);
    }

    #[test]
    fn mixed_script_detection_cyrillic_in_latin() {
        // Cyrillic 'р' (U+0440) mixed with Latin 'assword'
        let input = "\u{0440}assword";
        let warnings = UnicodeNormalizer::detect_mixed_script(input);
        assert!(!warnings.is_empty(), "Should detect Cyrillic in Latin context");
        assert_eq!(warnings[0].character, '\u{0440}');
    }

    #[test]
    fn mixed_script_detection_latin_in_cyrillic() {
        // Mostly Cyrillic with a Latin 'a' snuck in
        let input = "\u{043F}\u{0430}\u{0440}\u{043E}\u{043B}\u{044C}a";
        let warnings = UnicodeNormalizer::detect_mixed_script(input);
        assert!(!warnings.is_empty(), "Should detect Latin in Cyrillic context");
    }

    #[test]
    fn pure_cjk_no_mixed_script_warning() {
        let input = "你好世界測試";
        let warnings = UnicodeNormalizer::detect_mixed_script(input);
        assert!(
            warnings.is_empty(),
            "Pure CJK text should produce no warnings"
        );
    }

    #[test]
    fn grapheme_cluster_limit_enforced() {
        // A long emoji combination: family emoji with multiple ZWJ
        // 👨‍👩‍👧‍👦 = U+1F468 U+200D U+1F469 U+200D U+1F467 U+200D U+1F466
        let input = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
        let (result, warnings) = UnicodeNormalizer::enforce_grapheme_cluster_limit(input, 4);
        // The grapheme cluster has 7 chars, should be truncated to 4
        assert!(result.chars().count() <= 4);
        assert!(!warnings.is_empty());
        if let SecurityWarning::GraphemeClusterTruncated { original_len, .. } = &warnings[0] {
            assert_eq!(*original_len, 7);
        } else {
            panic!("Expected GraphemeClusterTruncated warning");
        }
    }

    #[test]
    fn grapheme_cluster_limit_short_clusters_pass() {
        let input = "hello";
        let (result, warnings) = UnicodeNormalizer::enforce_grapheme_cluster_limit(input, 8);
        assert_eq!(result, "hello");
        assert!(warnings.is_empty());
    }

    #[test]
    fn cjk_fullwidth_preserve_mode() {
        // Fullwidth 'Ａ' (U+FF21) should be preserved; regular compatibility chars normalized
        let input = "\u{FF21}\u{2126}"; // Fullwidth A + Ohm sign (compat → Ω)
        let result = UnicodeNormalizer::normalize_nfkc_preserve_cjk_fullwidth(input);
        // Fullwidth A preserved, Ohm sign normalized to Greek capital omega
        assert!(result.starts_with('\u{FF21}'));
        assert!(result.contains('\u{03A9}')); // Greek capital omega
    }

    #[test]
    fn full_sanitize_pipeline() {
        // Input with ANSI escapes, invisible chars, and normal text
        let input = "\x1b[31m\u{200B}hello\u{FE0F} world\x1b[0m";
        let config = SanitizeConfig::default();
        let result = UnicodeNormalizer::sanitize(input, &config);

        assert!(!result.sanitized.contains('\x1b'));
        assert!(!result.sanitized.contains('\u{200B}'));
        assert!(!result.sanitized.contains('\u{FE0F}'));
        assert!(result.sanitized.contains("hello"));
        assert!(result.sanitized.contains("world"));
        assert!(result.chars_removed > 0);
        assert!(!result.warnings.is_empty());
    }

    #[test]
    fn chars_removed_count_accurate() {
        let input = "\u{200B}\u{200C}\u{200D}abc";
        let config = SanitizeConfig {
            strip_invisible: true,
            detect_mixed_script: false,
            ..SanitizeConfig::default()
        };
        let result = UnicodeNormalizer::sanitize(input, &config);
        assert_eq!(result.chars_removed, 3);
        assert_eq!(result.sanitized, "abc");
    }

    #[test]
    fn empty_input() {
        let result = UnicodeNormalizer::sanitize("", &SanitizeConfig::default());
        assert_eq!(result.sanitized, "");
        assert!(result.warnings.is_empty());
        assert_eq!(result.chars_removed, 0);
    }

    #[test]
    fn pure_ascii_no_changes() {
        let input = "Hello, world! 123";
        let result = UnicodeNormalizer::sanitize(input, &SanitizeConfig::default());
        assert_eq!(result.sanitized, input);
        assert_eq!(result.chars_removed, 0);
    }

    #[test]
    fn cjk_fullwidth_preserved_in_default_config() {
        // Default config has cjk_fullwidth_preserve: true and normalization: Nfkc
        // Fullwidth chars should be preserved even under NFKC
        let input = "\u{FF01}\u{3000}\u{FF21}"; // ！ ideographic space  Ａ
        let config = SanitizeConfig::default();
        let result = UnicodeNormalizer::sanitize(input, &config);
        assert!(result.sanitized.contains('\u{FF01}'));
        assert!(result.sanitized.contains('\u{3000}'));
        assert!(result.sanitized.contains('\u{FF21}'));
    }

    #[test]
    fn strip_invisible_disabled() {
        let input = "hello\u{200B}world";
        let config = SanitizeConfig {
            strip_invisible: false,
            ..SanitizeConfig::default()
        };
        let result = UnicodeNormalizer::sanitize(input, &config);
        assert!(result.sanitized.contains('\u{200B}'));
    }
}
