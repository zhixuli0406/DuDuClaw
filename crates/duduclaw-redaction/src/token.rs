//! Redaction token type — `<REDACT:CATEGORY:HASH32>`.
//!
//! The token format is intentionally LLM-friendly:
//! - bracketed (`<` / `>`) so models tend not to truncate or paraphrase
//! - the category survives in plain text so the model retains type context
//!   ("this is an email address")
//! - the 32-hex hash carries 128 bits of session-scoped entropy. A short
//!   (32-bit) hash had a ~50% birthday-collision chance at ~77k distinct
//!   values; a collision lets one vault mapping clobber another, leaking
//!   cross-entity PII or losing data. 128 bits makes that negligible.

use std::fmt;
use std::str::FromStr;

use ring::hmac;
use serde::{Deserialize, Serialize};

use crate::error::{RedactionError, Result};

/// Token wire prefix.
pub const TOKEN_PREFIX: &str = "<REDACT:";
/// Token wire suffix.
pub const TOKEN_SUFFIX: &str = ">";
/// Hash length in hex chars (== 16 bytes / 128 bits of HMAC output).
pub const TOKEN_HASH_LEN: usize = 32;
/// Maximum category string length.
pub const CATEGORY_MAX_LEN: usize = 32;

/// A redaction token.
///
/// Internally stored as the canonical wire string `<REDACT:CAT:hash>` so
/// `Display`, comparisons, and serialisation are trivially equal to the
/// in-text form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Token(String);

impl Token {
    /// Construct a token from a `(category, hash)` pair. Both inputs are
    /// validated: category must be A-Z/0-9/`_`, hash must be exactly
    /// 32 lowercase hex chars.
    pub fn new(category: &str, hash: &str) -> Result<Self> {
        validate_category(category)?;
        validate_hash(hash)?;
        Ok(Token(format!("{TOKEN_PREFIX}{category}:{hash}{TOKEN_SUFFIX}")))
    }

    /// Try to parse a token from its wire form. Returns `None` if the input
    /// does not look like a token; returns `Err` if the prefix/suffix match
    /// but the body is malformed.
    pub fn parse(s: &str) -> Option<Self> {
        let body = s.strip_prefix(TOKEN_PREFIX)?.strip_suffix(TOKEN_SUFFIX)?;
        let (category, hash) = body.split_once(':')?;
        validate_category(category).ok()?;
        validate_hash(hash).ok()?;
        Some(Token(s.to_string()))
    }

    /// Token category (e.g. `"EMAIL"`).
    pub fn category(&self) -> &str {
        let body = &self.0[TOKEN_PREFIX.len()..self.0.len() - TOKEN_SUFFIX.len()];
        body.split_once(':').map(|(c, _)| c).unwrap_or("")
    }

    /// Token hash (32 lowercase hex chars).
    pub fn hash(&self) -> &str {
        let body = &self.0[TOKEN_PREFIX.len()..self.0.len() - TOKEN_SUFFIX.len()];
        body.split_once(':').map(|(_, h)| h).unwrap_or("")
    }

    /// Raw wire string. Identical to `Display`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Token {
    type Err = RedactionError;

    fn from_str(s: &str) -> Result<Self> {
        Token::parse(s).ok_or_else(|| RedactionError::InvalidToken(s.to_string()))
    }
}

fn validate_category(category: &str) -> Result<()> {
    if category.is_empty() || category.len() > CATEGORY_MAX_LEN {
        return Err(RedactionError::InvalidToken(format!(
            "category length out of range: {}",
            category.len()
        )));
    }
    if !category
        .bytes()
        .all(|b| matches!(b, b'A'..=b'Z' | b'0'..=b'9' | b'_'))
    {
        return Err(RedactionError::InvalidToken(format!(
            "category contains invalid characters: {category}"
        )));
    }
    Ok(())
}

fn validate_hash(hash: &str) -> Result<()> {
    if hash.len() != TOKEN_HASH_LEN {
        return Err(RedactionError::InvalidToken(format!(
            "hash length must be {TOKEN_HASH_LEN}, got {}",
            hash.len()
        )));
    }
    if !hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(RedactionError::InvalidToken(format!(
            "hash must be lowercase hex: {hash}"
        )));
    }
    Ok(())
}

/// Number of HMAC output bytes used for the session hash (16 bytes = 128 bits).
const SESSION_HASH_BYTES: usize = TOKEN_HASH_LEN / 2;

/// Compute the 32-hex-char session hash for a given (salt, original) pair.
///
/// Uses HMAC-SHA256 truncated to the first 16 bytes (128 bits), encoded as
/// lowercase hex. The same salt + original always produce the same hash, which
/// is what allows redact-side determinism (same value within a session → same
/// token). 128 bits keeps the birthday-collision probability negligible even
/// for very large vaults, preventing one mapping from clobbering another.
pub fn session_hash(salt: &[u8], original: &[u8]) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, salt);
    let sig = hmac::sign(&key, original);
    let bytes = &sig.as_ref()[..SESSION_HASH_BYTES];
    hex_lower(bytes)
}

/// Derive the per-session salt for an agent. The salt is the full
/// HMAC-SHA256(agent_key, "session:" + session_id) output, used by
/// [`session_hash`] for per-session tokens.
pub fn derive_session_salt(agent_key: &[u8], session_id: &str) -> [u8; 32] {
    let key = hmac::Key::new(hmac::HMAC_SHA256, agent_key);
    let sig = hmac::sign(&key, format!("session:{session_id}").as_bytes());
    let mut out = [0u8; 32];
    out.copy_from_slice(sig.as_ref());
    out
}

/// Derive the cross-session stable salt for an agent — used by rules
/// marked `cross_session_stable = true` so the same organisational
/// vocabulary (project codenames, team aliases) gets a consistent token
/// across conversations.
pub fn derive_stable_salt(agent_key: &[u8]) -> [u8; 32] {
    let key = hmac::Key::new(hmac::HMAC_SHA256, agent_key);
    let sig = hmac::sign(&key, b"stable");
    let mut out = [0u8; 32];
    out.copy_from_slice(sig.as_ref());
    out
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // A valid 32-hex-char hash literal for tests.
    const H32: &str = "a3f9b2c10123abcd0123abcda3f9b2c1";

    #[test]
    fn new_and_display_round_trip() {
        let t = Token::new("EMAIL", H32).unwrap();
        assert_eq!(t.to_string(), format!("<REDACT:EMAIL:{H32}>"));
        assert_eq!(t.category(), "EMAIL");
        assert_eq!(t.hash(), H32);
    }

    #[test]
    fn parse_valid_token() {
        let t = Token::parse(&format!("<REDACT:CUSTOMER_EMAIL:{H32}>")).unwrap();
        assert_eq!(t.category(), "CUSTOMER_EMAIL");
        assert_eq!(t.hash(), H32);
    }

    #[test]
    fn parse_rejects_bad_prefix() {
        assert!(Token::parse(&format!("REDACT:EMAIL:{H32}")).is_none());
        assert!(Token::parse(&format!("<REDACTX:EMAIL:{H32}>")).is_none());
    }

    #[test]
    fn parse_rejects_bad_hash() {
        // Too short (the old 8-char width is no longer valid).
        assert!(Token::parse("<REDACT:EMAIL:a3f9b2c1>").is_none());
        assert!(Token::parse("<REDACT:EMAIL:short>").is_none());
        // Uppercase / non-hex chars at the correct length.
        assert!(Token::parse("<REDACT:EMAIL:A3F9B2C10123ABCD0123ABCDA3F9B2C1>").is_none());
        assert!(Token::parse("<REDACT:EMAIL:zz337799zz337799zz337799zz337799>").is_none());
    }

    #[test]
    fn parse_rejects_bad_category() {
        assert!(Token::parse(&format!("<REDACT:lowercase:{H32}>")).is_none());
        assert!(Token::parse(&format!("<REDACT:HAS SPACE:{H32}>")).is_none());
        assert!(Token::parse(&format!("<REDACT::{H32}>")).is_none());
    }

    #[test]
    fn session_hash_is_deterministic_per_salt() {
        let salt = b"some-salt-bytes-32-bytes-long-xx";
        let h1 = session_hash(salt, b"alice@acme.com");
        let h2 = session_hash(salt, b"alice@acme.com");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), TOKEN_HASH_LEN);
    }

    #[test]
    fn session_hash_is_128_bits() {
        // 32 hex chars == 16 bytes == 128 bits of entropy. This is the
        // core defence against birthday collisions in the vault.
        let h = session_hash(b"some-salt-bytes-32-bytes-long-xx", b"value");
        assert_eq!(h.len(), 32);
        assert_eq!(TOKEN_HASH_LEN, 32);
        assert!(h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    }

    #[test]
    fn session_hash_differs_across_salts() {
        let h1 = session_hash(b"salt-A--padding-to-make-it-32byt", b"alice@acme.com");
        let h2 = session_hash(b"salt-B--padding-to-make-it-32byt", b"alice@acme.com");
        assert_ne!(h1, h2);
    }

    #[test]
    fn from_str_works() {
        let t: Token = format!("<REDACT:EMAIL:{H32}>").parse().unwrap();
        assert_eq!(t.category(), "EMAIL");
    }

    #[test]
    fn new_rejects_invalid_inputs() {
        assert!(Token::new("lower", H32).is_err());
        assert!(Token::new("EMAIL", "tooshort").is_err());
        assert!(Token::new("EMAIL", "abcdef01").is_err()); // old 8-char width rejected
        assert!(Token::new("EMAIL", "ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ").is_err());
        assert!(Token::new("", H32).is_err());
    }
}
