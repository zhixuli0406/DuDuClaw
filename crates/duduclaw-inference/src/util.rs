//! Shared utility functions.

use std::path::PathBuf;

/// Expand `~` or `~/...` prefix to the HOME directory.
/// Uses `HOME` on Unix, `USERPROFILE` on Windows.
pub fn expand_tilde(path: &str) -> PathBuf {
    let home = || std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"));
    if path == "~" {
        if let Ok(h) = home() {
            return PathBuf::from(h);
        }
    } else if let Some(rest) = path.strip_prefix("~/")
        && let Ok(h) = home() {
            return PathBuf::from(h).join(rest);
        }
    PathBuf::from(path)
}

/// Rough token estimation (1 token ≈ 4 chars English, 1.5 chars CJK).
pub fn estimate_tokens(text: &str) -> usize {
    let mut cjk_chars: usize = 0;
    let mut total_chars: usize = 0;
    for c in text.chars() {
        total_chars += 1;
        if is_cjk(c) {
            cjk_chars += 1;
        }
    }
    let non_cjk_chars = total_chars - cjk_chars;
    let cjk_tokens = (cjk_chars as f64 / 1.5).ceil() as usize;
    let ascii_tokens = non_cjk_chars / 4;
    (cjk_tokens + ascii_tokens).max(1)
}

/// Check if a character is CJK.
pub fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}' |
        '\u{3400}'..='\u{4DBF}' |
        '\u{3000}'..='\u{303F}' |
        '\u{3040}'..='\u{309F}' |
        '\u{30A0}'..='\u{30FF}' |
        '\u{AC00}'..='\u{D7AF}'
    )
}
