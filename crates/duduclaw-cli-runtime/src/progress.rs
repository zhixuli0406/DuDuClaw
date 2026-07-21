//! Substantive-progress detection for the interactive REPL invoke loop.
//!
//! The interactive path historically failed a turn only on a single fixed
//! wall-clock deadline (180 s), which false-killed long tasks (long tool calls,
//! multi-minute agentic work). This module replaces that with **stall
//! detection**: track the
//! moment of the last *substantive progress* and fail only after an idle window
//! elapses with no progress, with a generous absolute hard cap as the safety net.
//!
//! ## Calibration (live Claude Code 2.1.173 PTY captures, 2026-07-21)
//!
//! Two real capture runs shaped the rule (raw data in the change report):
//!
//! - **Thinking / streaming phase** — the ANSI-stripped rolling buffer GROWS
//!   every frame purely from spinner redraws (measured 31 → 1968 bytes across
//!   70 s of a thinking phase that emitted zero answer text). So the raw buffer
//!   length is NOT a progress signal. What *does* move is the token counter
//!   (`N tokens` / `N.Nk tokens`): measured 112 → 538 → 1.0k while thinking.
//!
//! - **Tool-call + completion** — the token counter froze at 104 for ~33 s
//!   while a `sleep` bash tool ran (genuine idle, not a wedge), then advanced to
//!   521 → 522 as the answer streamed. Crucially, once the answer landed and the
//!   closing sentinel arrived, BOTH the token counter (522) and the de-noised
//!   content stayed byte-for-byte constant for 20 s straight — the exact
//!   signature of a wedged/completed REPL.
//!
//! - The **elapsed timer** (`(12s · …)`) ticks every second even when the model
//!   is wedged, so it must be excluded from the progress signal — as must the
//!   spinner glyphs, which redraw continuously.
//!
//! ## Rule
//!
//! `substantive progress` = the parsed **max token count increased** OR the
//! **de-noised prose content changed** (length or checksum). De-noising drops
//! spinner glyphs, digits (kills timer/token-number churn), separator runs, and
//! any status line mentioning tokens / the elapsed timer / "thinking" /
//! "esc to interrupt". What remains is prose letters, which stay constant across
//! pure spinner redraws but grow when real answer text or tool-activity lines
//! appear. The token counter carries the signal during the (prose-less) thinking
//! phase; the prose signal carries it during streaming / tool activity.

use crate::envelope::strip_ansi;

/// Spinner / activity glyphs the Claude TUI animates. Kept in sync with the
/// chrome set in [`crate::envelope`]; extended with the meter/arrow glyphs seen
/// in the 2026-07-21 tool-activity banner.
const SPINNER_GLYPHS: &[char] = &[
    '✶', '✳', '✢', '✻', '✽', '✺', '✷', '⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧',
    '⠇', '⠏', '⏺', '✔', '✓', '·', '●', '○', '↑', '↓', '│', '❯', '←', '⏵',
];

/// A comparable snapshot of "how much substantive output has appeared so far".
///
/// Progress is monotone in spirit but we compare all three fields so a change in
/// any one counts (token counters can reset between segments — observed 1.0k →
/// 502 — so we compare max, and prose can change without changing length).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProgressSignature {
    /// Highest `N tokens` value parsed from the whole buffer.
    pub max_tokens: u64,
    /// Count of prose letters on non-noise lines.
    pub prose_len: usize,
    /// Order-sensitive checksum of the prose letters (catches same-length edits).
    pub prose_checksum: u64,
}

impl ProgressSignature {
    /// Compute the signature from a RAW (not-yet-ANSI-stripped) buffer.
    pub fn from_raw(raw: &str) -> Self {
        Self::from_stripped(&strip_ansi(raw))
    }

    /// Compute the signature from an already-ANSI-stripped buffer.
    pub fn from_stripped(stripped: &str) -> Self {
        let max_tokens = parse_max_tokens(stripped);
        let (prose_len, prose_checksum) = prose_fingerprint(stripped);
        Self {
            max_tokens,
            prose_len,
            prose_checksum,
        }
    }

    /// True when `self` represents substantive progress beyond `prev`.
    ///
    /// Token counts only count as progress when they INCREASE (a reset to a
    /// lower value is a new segment starting, not a wedge — but it also isn't
    /// evidence of forward motion, so we let the prose signal decide that turn).
    pub fn advanced_from(&self, prev: &ProgressSignature) -> bool {
        self.max_tokens > prev.max_tokens
            || self.prose_len != prev.prose_len
            || self.prose_checksum != prev.prose_checksum
    }
}

/// Parse the largest `N tokens` / `N.Nk tokens` / `N.Nm tokens` count anywhere
/// in `s`. Dependency-free scan (no regex crate in this crate). Returns 0 when
/// no token counter is present.
pub fn parse_max_tokens(s: &str) -> u64 {
    let mut max = 0u64;
    // Find each occurrence of "tokens" and look backwards for the number.
    let bytes = s.as_bytes();
    let mut search_from = 0usize;
    while let Some(rel) = s[search_from..].find("tokens") {
        let kw_start = search_from + rel;
        search_from = kw_start + "tokens".len();
        // Walk left over spaces to the number token.
        let mut i = kw_start;
        while i > 0 && bytes[i - 1] == b' ' {
            i -= 1;
        }
        // Optional k/m suffix immediately before "tokens".
        let mut mult: u64 = 1;
        if i > 0 {
            let c = bytes[i - 1] | 0x20; // ascii-lower
            if c == b'k' {
                mult = 1_000;
                i -= 1;
            } else if c == b'm' {
                mult = 1_000_000;
                i -= 1;
            }
        }
        while i > 0 && bytes[i - 1] == b' ' {
            i -= 1;
        }
        // Collect the numeric run (digits + one optional '.') ending at i.
        let num_end = i;
        let mut j = i;
        let mut seen_dot = false;
        while j > 0 {
            let c = bytes[j - 1];
            if c.is_ascii_digit() {
                j -= 1;
            } else if c == b'.' && !seen_dot {
                seen_dot = true;
                j -= 1;
            } else {
                break;
            }
        }
        if j < num_end {
            if let Ok(val) = s[j..num_end].parse::<f64>() {
                let v = (val * mult as f64) as u64;
                max = max.max(v);
            }
        }
    }
    max
}

/// Fingerprint the de-noised prose content: `(letter_count, order_checksum)`.
///
/// A line is dropped when it is pure chrome (spinner/timer/token status /
/// separators). From surviving lines we keep only alphabetic characters
/// (ASCII + Unicode letters, so CJK counts), which strips digit churn from the
/// elapsed timer and any leaked spinner counters.
pub fn prose_fingerprint(stripped: &str) -> (usize, u64) {
    let mut len = 0usize;
    let mut checksum = 0u64;
    for line in stripped.lines() {
        if is_noise_line(line) {
            continue;
        }
        for ch in line.chars() {
            if ch.is_alphabetic() {
                len += 1;
                // Order-sensitive rolling checksum (FNV-ish).
                checksum = checksum
                    .wrapping_mul(1099511628211)
                    .wrapping_add(ch as u64);
            }
        }
    }
    (len, checksum)
}

/// True when a line is TUI chrome that redraws without representing progress:
/// pure spinner, the elapsed-timer/token status parenthetical, separators, or a
/// known status phrase.
pub fn is_noise_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }
    // After dropping spinner glyphs + whitespace, an empty remainder = pure
    // spinner line.
    let core: String = trimmed
        .chars()
        .filter(|c| !SPINNER_GLYPHS.contains(c) && !c.is_whitespace())
        .collect();
    if core.is_empty() {
        return true;
    }
    // Separator-only line.
    if trimmed
        .chars()
        .all(|c| matches!(c, '─' | '━' | '-' | '=' | '·' | '•' | '│') || c.is_whitespace())
    {
        return true;
    }
    // Elapsed-timer parenthetical: an "(" followed (after optional glyphs/
    // spaces) by digits then 's'. Cheap check: a "(" with "Ns" nearby.
    if has_elapsed_timer(trimmed) {
        return true;
    }
    let low: String = trimmed
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    const STATUS: &[&str] = &[
        "tokens",
        "esctointerrupt",
        "estointerrup", // TUI can eat chars: "es to interrup"
        "?forshortcuts",
        "forshortcuts",
        "shift+tab",
        "thinking",
        "cogitat", // "Cogitating" / "Cogitated for Ns"
        "pondering",
        "inferring",
        "recombobulating",
    ];
    STATUS.iter().any(|m| low.contains(m))
}

/// Detect an elapsed-timer fragment like `(12s`, tolerant of a spinner glyph or
/// spaces between `(` and the digits.
fn has_elapsed_timer(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    for (idx, &c) in chars.iter().enumerate() {
        if c != '(' {
            continue;
        }
        // Scan forward skipping spaces/spinner glyphs to the first digit run.
        let mut k = idx + 1;
        while k < chars.len()
            && (chars[k].is_whitespace() || SPINNER_GLYPHS.contains(&chars[k]))
        {
            k += 1;
        }
        let digit_start = k;
        while k < chars.len() && chars[k].is_ascii_digit() {
            k += 1;
        }
        if k > digit_start && k < chars.len() && chars[k] == 's' {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tokens_plain_and_suffixed() {
        assert_eq!(parse_max_tokens("↓ 112 tokens · thinking"), 112);
        assert_eq!(parse_max_tokens("↓ 1.0k tokens"), 1000);
        assert_eq!(parse_max_tokens("2.5m tokens used"), 2_500_000);
        // Takes the MAX across occurrences.
        assert_eq!(parse_max_tokens("50 tokens ... 538 tokens ... 12 tokens"), 538);
        assert_eq!(parse_max_tokens("no counter here"), 0);
    }

    #[test]
    fn noise_lines_are_dropped() {
        assert!(is_noise_line("✻ ✶ ✳"));
        assert!(is_noise_line("────────────────"));
        assert!(is_noise_line("✳ Cogitating (12s · ↓ 538 tokens · thinking with high effort)"));
        assert!(is_noise_line("· esc to interrupt"));
        assert!(is_noise_line("? for shortcuts"));
        assert!(is_noise_line("   "));
        // Real content is NOT noise.
        assert!(!is_noise_line("Ocean currents are driven by wind and density."));
        assert!(!is_noise_line("DONE"));
    }

    #[test]
    fn elapsed_timer_detected_even_with_leading_glyph() {
        assert!(has_elapsed_timer("Cogitating (12s"));
        assert!(has_elapsed_timer("(✳ 3s ·"));
        assert!(!has_elapsed_timer("(hello world)"));
    }

    #[test]
    fn pure_spinner_frames_do_not_advance() {
        // Two different spinner frames with a ticking timer + flat token count:
        // the classic wedge signature. Signatures must be EQUAL.
        let a = "The answer so far.\n✻ Cogitating (11s · ↓ 104 tokens · thinking)";
        let b = "The answer so far.\n✶ Cogitating (25s · ↓ 104 tokens · thinking)";
        let sa = ProgressSignature::from_stripped(a);
        let sb = ProgressSignature::from_stripped(b);
        assert!(!sb.advanced_from(&sa), "spinner+timer churn must NOT be progress");
    }

    #[test]
    fn rising_token_count_is_progress() {
        let a = "✻ Cogitating (11s · ↓ 104 tokens · thinking)";
        let b = "✶ Cogitating (14s · ↓ 260 tokens · thinking)";
        let sa = ProgressSignature::from_stripped(a);
        let sb = ProgressSignature::from_stripped(b);
        assert!(sb.advanced_from(&sa), "token counter rising IS progress");
    }

    #[test]
    fn new_prose_is_progress() {
        let a = "Ocean currents\n✻ (3s · ↓ 50 tokens)";
        let b = "Ocean currents are driven by wind\n✶ (5s · ↓ 50 tokens)";
        let sa = ProgressSignature::from_stripped(a);
        let sb = ProgressSignature::from_stripped(b);
        assert!(sb.advanced_from(&sa), "new answer prose IS progress");
    }

    #[test]
    fn frozen_completed_screen_does_not_advance() {
        // Live-captured freeze: identical buffer for 20 s once the answer landed.
        let frozen = "itcode 0) DONE  =====DUDUCLAW.MARK=====\n✻ Cogitated for 58s";
        let s1 = ProgressSignature::from_stripped(frozen);
        let s2 = ProgressSignature::from_stripped(frozen);
        assert!(!s2.advanced_from(&s1));
    }

    #[test]
    fn raw_buffer_growth_from_spinner_is_not_progress() {
        // Mirrors the measured 31→1968 byte growth: same prose, more appended
        // spinner frames. Must NOT count as progress.
        let early = "Working on it\n✻";
        let mut late = String::from("Working on it\n");
        for _ in 0..200 {
            late.push_str("✻ ✶ ✳ ✢ (12s · ↓ 0 tokens · thinking)\n");
        }
        let se = ProgressSignature::from_stripped(early);
        let sl = ProgressSignature::from_stripped(&late);
        assert!(
            !sl.advanced_from(&se),
            "accumulated spinner redraws must not register as progress"
        );
    }
}
