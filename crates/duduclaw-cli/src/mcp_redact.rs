// mcp_redact.rs — Log redaction for DuDuClaw API keys (W19-P0)
//
// Masks any DuDuClaw API key found in a string.
// Key pattern: ddc_[a-z]+_[a-f0-9]{32}
// Masked form: ddc_***_<first4>...<last4>

use std::borrow::Cow;

// ── Regex constants ───────────────────────────────────────────────────────────

// Matches a real (un-redacted) DuDuClaw key:
//   ddc_<env>_<32 hex chars>
// The env segment is [a-z]+ (lowercase letters only).
// The hex segment is exactly 32 chars of [a-f0-9].
//
// Must NOT match already-redacted form "ddc_***_xxxx...yyyy".
const KEY_PATTERN: &str = r"ddc_[a-z]+_([a-f0-9]{32})";

/// Redact all DuDuClaw API keys in `input`.
///
/// Returns `Cow::Borrowed` (zero allocation) when no keys are found,
/// and `Cow::Owned` when at least one key was replaced.
pub fn redact(input: &str) -> Cow<'_, str> {
    let re = regex::Regex::new(KEY_PATTERN).unwrap();

    // Fast path: no match at all → zero-copy return
    if !re.is_match(input) {
        return Cow::Borrowed(input);
    }

    let result = re.replace_all(input, |caps: &regex::Captures| {
        let full_match = caps.get(0).unwrap().as_str();
        let hex = caps.get(1).unwrap().as_str(); // the 32-char hex portion

        // Guard: if this looks like an already-redacted form, leave it alone.
        // An already-redacted form has "_***_" in it.
        if full_match.contains("_***_") {
            return full_match.to_string();
        }

        // full_match is like "ddc_prod_<32hex>"
        let first4 = &hex[..4];
        let last4 = &hex[hex.len() - 4..];
        format!("ddc_***_{first4}...{last4}")
    });

    Cow::Owned(result.into_owned())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Test 1: complete key is redacted correctly ────────────────────────────
    #[test]
    fn test_complete_key_redacted() {
        let input = "ddc_prod_a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4";
        let output = redact(input);
        assert_eq!(output.as_ref(), "ddc_***_a1b2...c3d4");
    }

    // ── Test 2: no key → Cow::Borrowed (zero-copy) ───────────────────────────
    #[test]
    fn test_no_key_returns_borrowed() {
        let input = "no api key here, just plain text";
        let output = redact(input);
        assert!(
            matches!(output, Cow::Borrowed(_)),
            "expected Cow::Borrowed for input with no keys"
        );
    }

    // ── Test 3: multiple keys all redacted ───────────────────────────────────
    #[test]
    fn test_multiple_keys_all_redacted() {
        let input = "key1=ddc_prod_a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4 key2=ddc_dev_ffffffffffffffffffffffffffffffff";
        let output = redact(input);
        assert!(
            output.contains("ddc_***_a1b2...c3d4"),
            "first key should be redacted"
        );
        assert!(
            output.contains("ddc_***_ffff...ffff"),
            "second key should be redacted"
        );
        // Neither original key should appear
        assert!(!output.contains("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4"));
        assert!(!output.contains("ffffffffffffffffffffffffffffffff"));
    }

    // ── Test 4: staging env key also redacted ────────────────────────────────
    #[test]
    fn test_staging_env_key_redacted() {
        let input = "ddc_staging_deadbeefdeadbeefdeadbeefdeadbeef";
        let output = redact(input);
        assert_eq!(output.as_ref(), "ddc_***_dead...beef");
    }

    // ── Test 5: key inside JSON string is redacted ───────────────────────────
    #[test]
    fn test_key_in_json_redacted() {
        let input = r#"{"api_key": "ddc_prod_a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4", "user": "alice"}"#;
        let output = redact(input);
        assert!(output.contains("ddc_***_a1b2...c3d4"));
        assert!(!output.contains("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4"));
    }

    // ── Test 6: already-redacted form is idempotent ───────────────────────────
    #[test]
    fn test_already_redacted_is_idempotent() {
        let redacted = "ddc_***_a1b2...c3d4";
        // "ddc_***_..." does NOT match the pattern ddc_[a-z]+_[a-f0-9]{32}
        // because "***" is not [a-z]+, so it stays unchanged.
        let output = redact(redacted);
        assert_eq!(output.as_ref(), redacted);
    }

    // ── Test 7: only 30 hex chars → not matched ───────────────────────────────
    #[test]
    fn test_30_hex_chars_not_redacted() {
        // 30 hex chars (2 short) — should not match
        let input = "ddc_prod_a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
        let output = redact(input);
        // Should be returned unchanged (Borrowed)
        assert!(
            matches!(output, Cow::Borrowed(_)),
            "30-char hex should not be matched and should return Borrowed"
        );
        assert_eq!(output.as_ref(), input);
    }
}
