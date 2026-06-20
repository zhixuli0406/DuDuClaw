//! Anchored / boundary-aware matching helpers.
//!
//! Several call sites historically used unanchored `contains` / `starts_with`
//! for security- or routing-relevant decisions, which let crafted inputs slip
//! through (`http://localhost.evil.com` matching `starts_with("http://localhost")`,
//! the keyword `hi` matching inside `this`, an agent id matching as a substring
//! of another). These helpers do whole-word / exact-authority matching instead.

/// ASCII case-insensitive whole-word containment.
///
/// Returns `true` when `needle` appears in `haystack` delimited on both sides by
/// a non-alphanumeric byte (or a string edge). Intended for ASCII keyword
/// matching (e.g. confidence-router fast keywords). `needle` is matched
/// case-insensitively over ASCII letters.
///
/// Empty `needle` returns `false` (a no-op keyword never matches).
pub fn word_contains_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let hay = haystack.as_bytes();
    let need = needle.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric();
    let eq_ci = |a: u8, b: u8| a.eq_ignore_ascii_case(&b);

    if need.len() > hay.len() {
        return false;
    }
    let last_start = hay.len() - need.len();
    for start in 0..=last_start {
        // Match the needle bytes case-insensitively.
        if !(0..need.len()).all(|i| eq_ci(hay[start + i], need[i])) {
            continue;
        }
        let before_ok = start == 0 || !is_word(hay[start - 1]);
        let after_idx = start + need.len();
        let after_ok = after_idx == hay.len() || !is_word(hay[after_idx]);
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

/// Exact-authority Origin matching for WebSocket / CORS allowlists.
///
/// `origin` is a browser `Origin` header value (`scheme://host[:port]`). Each
/// entry in `allowed` is either a bare `host` or `host:port`. A match requires
/// the origin's authority (everything after `scheme://`, up to the first `/`,
/// `?`, or `#`) to be *exactly equal* to an allowed entry, or its host portion
/// (authority with the `:port` stripped) to equal a port-less allowed entry.
///
/// This rejects `http://localhost.evil.com` against an allowlist of `localhost`,
/// which the old `origin.starts_with("http://localhost")` accepted.
pub fn origin_host_matches(origin: &str, allowed: &[&str]) -> bool {
    let authority = match origin_authority(origin) {
        Some(a) => a,
        None => return false,
    };
    let (origin_host, _) = host_and_has_port(authority);
    allowed.iter().any(|a| {
        if *a == authority {
            return true;
        }
        // A port-less allowed entry matches any port on the same host; a
        // port-qualified entry must match the authority exactly (handled above).
        let (allowed_host, allowed_has_port) = host_and_has_port(a);
        !allowed_has_port && allowed_host == origin_host
    })
}

/// Extract the authority (`host[:port]`) from an origin/URL string.
fn origin_authority(origin: &str) -> Option<&str> {
    let after_scheme = match origin.find("://") {
        Some(i) => &origin[i + 3..],
        // No scheme: treat the whole value as authority (still anchored below).
        None => origin,
    };
    if after_scheme.is_empty() {
        return None;
    }
    let end = after_scheme
        .find(|c| c == '/' || c == '?' || c == '#')
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..end];
    if authority.is_empty() {
        None
    } else {
        Some(authority)
    }
}

/// Split an authority into its host portion and whether a `:port` is present.
/// Handles bracketed IPv6 literals (`[::1]:8080` -> (`[::1]`, true);
/// `[::1]` -> (`[::1]`, false)) and bare IPv6 (`::1`, multiple colons, no port).
fn host_and_has_port(authority: &str) -> (&str, bool) {
    if let Some(close) = authority.find(']') {
        let host = &authority[..=close];
        let has_port = authority[close + 1..].starts_with(':');
        return (host, has_port);
    }
    // No brackets: a single colon delimits host:port; zero or many colons
    // (the latter being a bare IPv6 literal) means no port.
    if authority.bytes().filter(|b| *b == b':').count() == 1 {
        let i = authority.find(':').unwrap();
        (&authority[..i], true)
    } else {
        (authority, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_contains_rejects_substrings() {
        assert!(!word_contains_ci("this is realistic", "hi"));
        assert!(!word_contains_ci("realistic", "list"));
        assert!(word_contains_ci("say hi there", "hi"));
        assert!(word_contains_ci("make a list", "list"));
    }

    #[test]
    fn word_contains_is_case_insensitive() {
        assert!(word_contains_ci("Please LIST items", "list"));
        assert!(word_contains_ci("HI", "hi"));
    }

    #[test]
    fn word_contains_edges_and_empty() {
        assert!(word_contains_ci("list", "list"));
        assert!(!word_contains_ci("anything", ""));
        assert!(!word_contains_ci("ab", "abc"));
    }

    #[test]
    fn origin_exact_host_matches() {
        assert!(origin_host_matches("http://localhost", &["localhost"]));
        assert!(origin_host_matches("http://localhost:5173", &["localhost"]));
        assert!(origin_host_matches(
            "http://localhost:5173",
            &["localhost:5173"]
        ));
    }

    #[test]
    fn origin_rejects_suffix_attack() {
        assert!(!origin_host_matches("http://localhost.evil.com", &["localhost"]));
        assert!(!origin_host_matches(
            "http://127.0.0.1.evil.com",
            &["127.0.0.1"]
        ));
        assert!(!origin_host_matches("http://evil.com", &["localhost"]));
    }

    #[test]
    fn origin_port_specificity() {
        // Allowlist pins a port: a different port must not match.
        assert!(!origin_host_matches(
            "http://localhost:9999",
            &["localhost:5173"]
        ));
    }

    #[test]
    fn origin_ipv6_and_malformed() {
        assert!(origin_host_matches("http://[::1]:8080", &["[::1]"]));
        assert!(!origin_host_matches("", &["localhost"]));
        assert!(!origin_host_matches("http://", &["localhost"]));
    }
}
