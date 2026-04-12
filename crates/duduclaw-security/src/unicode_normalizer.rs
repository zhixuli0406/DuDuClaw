//! Unicode normalization and sanitization for security hardening.
//!
//! Defends against: invisible char injection (arXiv 2510.05025),
//! homoglyph/mixed-script attacks (arXiv 2508.14070, ACL GenAIDetect 2025),
//! emoji token explosion (tdcommons.org/dpubs_series/7836).

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use unicode_script::{Script, UnicodeScript};
use unicode_segmentation::UnicodeSegmentation;

/// Pre-compiled ANSI escape sequence regex (compiled once, reused across calls).
static ANSI_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\x1b\[[0-9;]*[a-zA-Z]|\x1b\][^\x07]*\x07|\x1b[^\[\]])")
        .expect("valid ANSI regex")
});

/// Configuration for Unicode sanitization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SanitizeConfig {
    /// Normalization form: "nfkc", "nfc", "nfkc_preserve_cjk_fullwidth"
    pub normalization: NormalizationMode,
    /// Strip invisible characters (Variation Selectors, Tag Chars, ZW chars, BiDi overrides)
    pub strip_invisible: bool,
    /// Detect mixed script usage (Latin+Cyrillic, Latin+Greek homoglyphs)
    pub detect_mixed_script: bool,
    /// Max grapheme cluster length (prevents emoji combo token explosion)
    pub grapheme_cluster_max: usize,
    /// Preserve CJK fullwidth characters during NFKC (important for zh-TW)
    pub cjk_fullwidth_preserve: bool,
}

impl Default for SanitizeConfig {
    fn default() -> Self {
        Self {
            normalization: NormalizationMode::Nfkc,
            strip_invisible: true,
            detect_mixed_script: true,
            grapheme_cluster_max: 8,
            cjk_fullwidth_preserve: true,
        }
    }
}

/// Unicode normalization mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NormalizationMode {
    /// NFKC normalization (canonical decomposition + compatibility composition)
    Nfkc,
    /// NFC normalization (canonical decomposition + canonical composition)
    Nfc,
    /// NFKC but preserve CJK fullwidth characters
    NfkcPreserveCjkFullwidth,
}

/// Warning about mixed script usage in input.
#[derive(Debug, Clone)]
pub struct MixedScriptWarning {
    /// Byte position in the input string
    pub position: usize,
    /// The offending character
    pub character: char,
    /// The dominant script detected in the input
    pub expected_script: String,
    /// The actual script of the offending character
    pub actual_script: String,
}

/// Security warning from sanitization.
#[derive(Debug, Clone)]
pub enum SecurityWarning {
    /// Mixed script characters detected (potential homoglyph attack)
    MixedScript(MixedScriptWarning),
    /// Invisible characters were removed
    InvisibleCharsRemoved { count: usize },
    /// A grapheme cluster was truncated due to exceeding max length
    GraphemeClusterTruncated {
        position: usize,
        original_len: usize,
    },
    /// ANSI escape sequences were removed
    AnsiEscapesRemoved { count: usize },
}

/// Result of sanitization.
#[derive(Debug, Clone)]
pub struct SanitizeResult {
    /// The sanitized string
    pub sanitized: String,
    /// Security warnings generated during sanitization
    pub warnings: Vec<SecurityWarning>,
    /// Total number of characters removed
    pub chars_removed: usize,
}

/// Stateless Unicode normalizer for security-critical input processing.
pub struct UnicodeNormalizer;

impl UnicodeNormalizer {
    /// Apply NFKC normalization (canonical decomposition + compatibility composition).
    pub fn normalize_nfkc(input: &str) -> String {
        input.nfkc().collect()
    }

    /// Apply NFC normalization (canonical decomposition + canonical composition).
    pub fn normalize_nfc(input: &str) -> String {
        input.nfc().collect()
    }

    /// Apply NFKC normalization but preserve CJK fullwidth characters.
    ///
    /// Fullwidth chars (U+FF01-U+FF60, U+FF5B-U+FF65, U+3000-U+303F) are extracted
    /// before NFKC, then reinserted at their original positions.
    pub fn normalize_nfkc_preserve_cjk_fullwidth(input: &str) -> String {
        // Collect chars with their byte positions, marking which are CJK fullwidth
        let chars: Vec<char> = input.chars().collect();
        let mut result = String::with_capacity(input.len());

        for &ch in &chars {
            if is_cjk_fullwidth(ch) {
                // Preserve fullwidth char as-is
                result.push(ch);
            } else {
                // Apply NFKC to individual non-fullwidth char
                let s: String = std::iter::once(ch).collect();
                result.push_str(&s.nfkc().collect::<String>());
            }
        }

        result
    }

    /// Strip invisible characters from input.
    ///
    /// Removes: Variation Selectors, Tag Characters, Zero-Width chars, BiDi overrides.
    /// Returns (cleaned string, count of characters removed).
    pub fn strip_invisible_chars(input: &str) -> (String, usize) {
        let mut removed = 0usize;
        let cleaned: String = input
            .chars()
            .filter(|&ch| {
                if is_invisible_char(ch) {
                    removed += 1;
                    false
                } else {
                    true
                }
            })
            .collect();
        (cleaned, removed)
    }

    /// Strip ANSI escape sequences from input.
    ///
    /// Returns (cleaned string, count of escape sequences removed).
    pub fn strip_ansi_escapes(input: &str) -> (String, usize) {
        let count = ANSI_RE.find_iter(input).count();
        let cleaned = ANSI_RE.replace_all(input, "").into_owned();
        (cleaned, count)
    }

    /// Detect mixed script usage in input text.
    ///
    /// Tracks the dominant script (ignoring Common/Inherited) and flags characters
    /// from confusable scripts (Latin+Cyrillic, Latin+Greek).
    pub fn detect_mixed_script(input: &str) -> Vec<MixedScriptWarning> {
        // First pass: determine dominant script by frequency
        let mut script_counts: std::collections::HashMap<Script, usize> =
            std::collections::HashMap::new();

        for ch in input.chars() {
            let script = ch.script();
            if script != Script::Common && script != Script::Inherited {
                *script_counts.entry(script).or_insert(0) += 1;
            }
        }

        let dominant = script_counts
            .iter()
            .max_by_key(|(_, count)| *count)
            .map(|(script, _)| *script);

        let dominant = match dominant {
            Some(s) => s,
            None => return Vec::new(), // All Common/Inherited, no warnings
        };

        // Confusable script pairs
        let is_confusable = |script: Script| -> bool {
            matches!(
                (dominant, script),
                (Script::Latin, Script::Cyrillic)
                    | (Script::Cyrillic, Script::Latin)
                    | (Script::Latin, Script::Greek)
                    | (Script::Greek, Script::Latin)
                    | (Script::Latin, Script::Armenian)
                    | (Script::Armenian, Script::Latin)
                    | (Script::Latin, Script::Georgian)
                    | (Script::Georgian, Script::Latin)
            )
        };

        // Second pass: flag deviations
        let mut warnings = Vec::new();
        let mut byte_pos = 0usize;

        for ch in input.chars() {
            let script = ch.script();
            if script != Script::Common && script != Script::Inherited && script != dominant
                && is_confusable(script) {
                    warnings.push(MixedScriptWarning {
                        position: byte_pos,
                        character: ch,
                        expected_script: format!("{:?}", dominant),
                        actual_script: format!("{:?}", script),
                    });
                }
            byte_pos += ch.len_utf8();
        }

        warnings
    }

    /// Enforce a maximum grapheme cluster length.
    ///
    /// Grapheme clusters exceeding `max_len` are truncated. This prevents emoji
    /// combination sequences from causing token explosion.
    pub fn enforce_grapheme_cluster_limit(
        input: &str,
        max_len: usize,
    ) -> (String, Vec<SecurityWarning>) {
        let mut result = String::with_capacity(input.len());
        let mut warnings = Vec::new();
        let mut byte_pos = 0usize;

        for grapheme in input.graphemes(true) {
            let char_count = grapheme.chars().count();
            if char_count > max_len {
                // Truncate to max_len chars
                let truncated: String = grapheme.chars().take(max_len).collect();
                warnings.push(SecurityWarning::GraphemeClusterTruncated {
                    position: byte_pos,
                    original_len: char_count,
                });
                result.push_str(&truncated);
            } else {
                result.push_str(grapheme);
            }
            byte_pos += grapheme.len();
        }

        (result, warnings)
    }

    /// Full sanitization pipeline.
    ///
    /// Order: strip_ansi -> strip_invisible -> normalize -> mixed_script_detect -> grapheme_limit.
    /// Aggregates all warnings and character removal counts.
    pub fn sanitize(input: &str, config: &SanitizeConfig) -> SanitizeResult {
        let mut warnings: Vec<SecurityWarning> = Vec::new();
        let mut total_removed = 0usize;
        let mut text = input.to_string();

        // Step 1: Strip ANSI escapes
        let (cleaned, ansi_count) = Self::strip_ansi_escapes(&text);
        if ansi_count > 0 {
            tracing::warn!(
                count = ansi_count,
                "Unicode sanitizer: stripped ANSI escape sequences"
            );
            warnings.push(SecurityWarning::AnsiEscapesRemoved { count: ansi_count });
            total_removed += ansi_count;
            text = cleaned;
        }

        // Step 2: Strip invisible characters
        if config.strip_invisible {
            let (cleaned, invisible_count) = Self::strip_invisible_chars(&text);
            if invisible_count > 0 {
                tracing::warn!(
                    count = invisible_count,
                    "Unicode sanitizer: stripped invisible characters"
                );
                warnings.push(SecurityWarning::InvisibleCharsRemoved {
                    count: invisible_count,
                });
                total_removed += invisible_count;
                text = cleaned;
            }
        }

        // Step 3: Normalize
        text = match config.normalization {
            NormalizationMode::Nfkc => {
                if config.cjk_fullwidth_preserve {
                    Self::normalize_nfkc_preserve_cjk_fullwidth(&text)
                } else {
                    Self::normalize_nfkc(&text)
                }
            }
            NormalizationMode::Nfc => Self::normalize_nfc(&text),
            NormalizationMode::NfkcPreserveCjkFullwidth => {
                Self::normalize_nfkc_preserve_cjk_fullwidth(&text)
            }
        };

        // Step 4: Detect mixed scripts
        if config.detect_mixed_script {
            let mixed_warnings = Self::detect_mixed_script(&text);
            if !mixed_warnings.is_empty() {
                tracing::warn!(
                    count = mixed_warnings.len(),
                    "Unicode sanitizer: mixed script characters detected"
                );
                for w in mixed_warnings {
                    warnings.push(SecurityWarning::MixedScript(w));
                }
            }
        }

        // Step 5: Enforce grapheme cluster limit
        let (cleaned, grapheme_warnings) =
            Self::enforce_grapheme_cluster_limit(&text, config.grapheme_cluster_max);
        if !grapheme_warnings.is_empty() {
            tracing::warn!(
                count = grapheme_warnings.len(),
                "Unicode sanitizer: grapheme clusters truncated"
            );
            warnings.extend(grapheme_warnings);
            text = cleaned;
        }

        SanitizeResult {
            sanitized: text,
            warnings,
            chars_removed: total_removed,
        }
    }
}

/// Check if a character is a CJK fullwidth character that should be preserved.
fn is_cjk_fullwidth(ch: char) -> bool {
    let cp = ch as u32;
    // Fullwidth ASCII variants and punctuation
    (0xFF01..=0xFF60).contains(&cp)
        // Fullwidth brackets and halfwidth/fullwidth forms
        || (0xFF5B..=0xFF65).contains(&cp)
        // CJK symbols and punctuation
        || (0x3000..=0x303F).contains(&cp)
}

/// Check if a character is an invisible/control character that should be stripped.
fn is_invisible_char(ch: char) -> bool {
    let cp = ch as u32;
    matches!(
        cp,
        // Variation Selectors (U+FE00-U+FE0F)
        0xFE00..=0xFE0F |
        // Variation Selectors Supplement (U+E0100-U+E01EF)
        0xE0100..=0xE01EF |
        // Tag Characters (U+E0000-U+E007F)
        0xE0000..=0xE007F |
        // Zero-Width chars
        0x200B | // ZWS
        0x200C | // ZWNJ
        0x200D | // ZWJ
        0xFEFF | // BOM / ZWNBSP
        // BiDi overrides (U+202A-U+202E)
        0x202A..=0x202E |
        // BiDi isolates (U+2066-U+2069)
        0x2066..=0x2069 |
        // LRM (Left-to-Right Mark)
        0x200E |
        // RLM (Right-to-Left Mark)
        0x200F |
        // Soft Hyphen
        0x00AD |
        // Word Joiner
        0x2060 |
        // Combining Grapheme Joiner
        0x034F |
        // Interlinear Annotation anchors (U+FFF9-U+FFFB)
        0xFFF9..=0xFFFB |
        // Deprecated format characters (U+206A-U+206F)
        0x206A..=0x206F
    )
}
