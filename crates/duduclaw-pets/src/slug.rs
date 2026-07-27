//! Slug sanitization for pet pack directory names.
//!
//! A slug is the on-disk folder name under `~/.duduclaw/pets/<slug>/`. It must be
//! filesystem-safe and path-traversal-proof while still being human-recognisable.
//! CJK display names are common (zh-TW users), so we do NOT strip non-ASCII —
//! we only reject path separators and dangerous characters, collapse whitespace
//! to `-`, and fall back to a stable id when nothing usable remains.

/// Sanitize a user-supplied display name into a filesystem-safe slug.
///
/// Rules:
/// - trim, lowercase ASCII (CJK unaffected),
/// - whitespace runs → single `-`,
/// - keep only alphanumerics (Unicode — so CJK survives) and `-`; every other
///   char (`.` `/` `~` `!` punctuation, control) is dropped → no path traversal,
///   no hidden dirs, no shell-surprising names,
/// - collapse repeated `-`, trim leading/trailing `-`,
/// - cap at 48 chars (char-safe, never mid-codepoint),
/// - empty result → `pet` (caller typically appends a unique suffix).
pub fn sanitize_slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.trim().chars() {
        if ch.is_whitespace() {
            if !prev_dash && !out.is_empty() {
                out.push('-');
                prev_dash = true;
            }
            continue;
        }
        if ch == '-' {
            out.push('-');
            prev_dash = true;
            continue;
        }
        if !ch.is_alphanumeric() {
            // Drop punctuation / symbols / control chars entirely.
            continue;
        }
        // Lowercase ASCII letters only; leave CJK/others verbatim.
        for c in ch.to_lowercase() {
            out.push(c);
        }
        prev_dash = false;
    }
    // Collapse any accidental double dashes and trim edges.
    let collapsed = collapse_dashes(&out);
    let trimmed = collapsed.trim_matches('-');
    let capped: String = trimmed.chars().take(48).collect();
    let capped = capped.trim_matches('-').to_string();
    if capped.is_empty() {
        "pet".to_string()
    } else {
        capped
    }
}

/// Collapse runs of `-` into a single `-`.
fn collapse_dashes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for ch in s.chars() {
        if ch == '-' {
            if !prev_dash {
                out.push('-');
            }
            prev_dash = true;
        } else {
            out.push(ch);
            prev_dash = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_lowercase_and_dash() {
        assert_eq!(sanitize_slug("My Cool Pet"), "my-cool-pet");
    }

    #[test]
    fn strips_path_traversal() {
        assert_eq!(sanitize_slug("../../etc/passwd"), "etcpasswd");
        assert_eq!(sanitize_slug("a/b\\c"), "abc");
        assert_eq!(sanitize_slug("..."), "pet");
        assert_eq!(sanitize_slug("~root"), "root");
    }

    #[test]
    fn preserves_cjk() {
        assert_eq!(sanitize_slug("小黑貓"), "小黑貓");
        assert_eq!(sanitize_slug("我的 寵物"), "我的-寵物");
    }

    #[test]
    fn collapses_and_trims_dashes() {
        assert_eq!(sanitize_slug("  --hello---world--  "), "hello-world");
        assert_eq!(sanitize_slug("!!!"), "pet");
    }

    #[test]
    fn caps_length_on_char_boundary() {
        let long = "貓".repeat(100);
        let slug = sanitize_slug(&long);
        assert_eq!(slug.chars().count(), 48);
        // Must be valid UTF-8 (never panics = boundary safe).
        assert!(slug.chars().all(|c| c == '貓'));
    }

    #[test]
    fn empty_and_whitespace() {
        assert_eq!(sanitize_slug(""), "pet");
        assert_eq!(sanitize_slug("   "), "pet");
    }
}
