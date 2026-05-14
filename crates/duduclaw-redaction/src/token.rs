//! Redaction token type — `<REDACT:CATEGORY:HASH8>`.
//!
//! The token format is intentionally LLM-friendly:
//! - bracketed (`<` / `>`) so models tend not to truncate or paraphrase
//! - the category survives in plain text so the model retains type context
//!   ("this is an email address")
//! - the 8-hex hash is short enough to fit alongside surrounding prose
//!   but long enough (~32 bits of session-scoped entropy) for distinctness.

use std::fmt;
use std::str::FromStr;

use ring::hmac;
use serde::{Deserialize, Serialize};

use crate::error::{RedactionError, Result};

/// Token wire prefix.
pub const TOKEN_PREFIX: &str = "<REDACT:";
/// Token wire suffix.
pub const TOKEN_SUFFIX: &str = ">";
/// Hash length in hex chars (== 4 bytes of HMAC output).
pub const TOKEN_HASH_LEN: usize = 8;
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
    /// 8 lowercase hex chars.
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

    /// Token hash (8 lowercase hex chars).
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

/// Compute the 8-hex-char session hash for a given (salt, original) pair.
///
/// Uses HMAC-SHA256 truncated to the first 4 bytes, encoded as lowercase hex.
/// The same salt + original always produce the same hash, which is what
/// allows redact-side determinism (same value within a session → same token).
pub fn session_hash(salt: &[u8], original: &[u8]) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, salt);
    let sig = hmac::sign(&key, original);
    let bytes = &sig.as_ref()[..4];
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

    #[test]
    fn new_and_display_round_trip() {
        let t = Token::new("EMAIL", "a3f9b2c1").unwrap();
        assert_eq!(t.to_string(), "<REDACT:EMAIL:a3f9b2c1>");
        assert_eq!(t.category(), "EMAIL");
        assert_eq!(t.hash(), "a3f9b2c1");
    }

    #[test]
    fn parse_valid_token() {
        let t = Token::parse("<REDACT:CUSTOMER_EMAIL:0123abcd>").unwrap();
        assert_eq!(t.category(), "CUSTOMER_EMAIL");
        assert_eq!(t.hash(), "0123abcd");
    }

    #[test]
    fn parse_rejects_bad_prefix() {
        assert!(Token::parse("REDACT:EMAIL:a3f9b2c1").is_none());
        assert!(Token::parse("<REDACTX:EMAIL:a3f9b2c1>").is_none());
    }

    #[test]
    fn parse_rejects_bad_hash() {
        assert!(Token::parse("<REDACT:EMAIL:short>").is_none());
        assert!(Token::parse("<REDACT:EMAIL:UPPERCASE>").is_none());
        assert!(Token::parse("<REDACT:EMAIL:zz337799>").is_none());
    }

    #[test]
    fn parse_rejects_bad_category() {
        assert!(Token::parse("<REDACT:lowercase:a3f9b2c1>").is_none());
        assert!(Token::parse("<REDACT:HAS SPACE:a3f9b2c1>").is_none());
        assert!(Token::parse("<REDACT::a3f9b2c1>").is_none());
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
    fn session_hash_differs_across_salts() {
        let h1 = session_hash(b"salt-A--padding-to-make-it-32byt", b"alice@acme.com");
        let h2 = session_hash(b"salt-B--padding-to-make-it-32byt", b"alice@acme.com");
        assert_ne!(h1, h2);
    }

    #[test]
    fn from_str_works() {
        let t: Token = "<REDACT:EMAIL:abcdef01>".parse().unwrap();
        assert_eq!(t.category(), "EMAIL");
    }

    #[test]
    fn new_rejects_invalid_inputs() {
        assert!(Token::new("lower", "abcdef01").is_err());
        assert!(Token::new("EMAIL", "tooshort").is_err());
        assert!(Token::new("EMAIL", "ZZZZZZZZ").is_err());
        assert!(Token::new("", "abcdef01").is_err());
    }
}
