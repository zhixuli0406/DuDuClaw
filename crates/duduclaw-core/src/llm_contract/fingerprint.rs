//! Stable fingerprints and canonical IDs.
//!
//! A [`Fingerprint`] identifies the *root cause* of a result across runs, so it
//! must be derived only from semantic fields that do not drift when the model
//! re-words or re-locates the same issue. A [`canonical_id`] is a lossless,
//! reversible ID built from a tuple of references (no slugging, so two
//! different tuples can never collide). Both mirror Cloudflare
//! security-audit-skill (MIT) `FINGERPRINT_PATTERN` and `canonicalCoverageId`.

use std::fmt;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use super::visible_text::{has_visible_content, is_forbidden_identifier_char};

/// Maximum fingerprint length in bytes.
pub const MAX_LEN: usize = 512;
/// Per-part cap on the readable slug in [`Fingerprint::derive`].
const SLUG_PART_MAX_CHARS: usize = 48;
/// Cap on the whole readable prefix, so any derived value stays under [`MAX_LEN`].
const PREFIX_MAX_CHARS: usize = 256;
/// Number of hex characters of the SHA-256 digest kept.
const HASH_HEX_CHARS: usize = 16;

/// A value matching `^[A-Za-z0-9][A-Za-z0-9._:/@+-]*$`, at most [`MAX_LEN`] bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fingerprint(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FingerprintError {
    Empty,
    InvalidLeadingCharacter,
    /// `index` is the byte index (all valid characters are ASCII, so the
    /// first invalid one is also at that character index).
    InvalidCharacter {
        index: usize,
    },
    TooLong {
        max: usize,
    },
}

impl fmt::Display for FingerprintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "fingerprint is empty"),
            Self::InvalidLeadingCharacter => {
                write!(f, "fingerprint must start with an ASCII letter or digit")
            }
            Self::InvalidCharacter { index } => write!(
                f,
                "fingerprint has a character outside [A-Za-z0-9._:/@+-] at index {index}"
            ),
            Self::TooLong { max } => write!(f, "fingerprint is longer than {max} bytes"),
        }
    }
}

impl std::error::Error for FingerprintError {}

fn is_fingerprint_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '@' | '+' | '-')
}

impl Fingerprint {
    pub fn parse(s: &str) -> Result<Self, FingerprintError> {
        let Some(first) = s.chars().next() else {
            return Err(FingerprintError::Empty);
        };
        if s.len() > MAX_LEN {
            return Err(FingerprintError::TooLong { max: MAX_LEN });
        }
        if !first.is_ascii_alphanumeric() {
            return Err(FingerprintError::InvalidLeadingCharacter);
        }
        if let Some((index, _)) = s.char_indices().find(|&(_, c)| !is_fingerprint_char(c)) {
            return Err(FingerprintError::InvalidCharacter { index });
        }
        Ok(Self(s.to_owned()))
    }

    /// Deterministic fingerprint `<readable prefix>@<first 16 hex of sha256>`.
    ///
    /// The prefix slugs each part (characters outside `[A-Za-z0-9._/-]` → `-`,
    /// runs of `-` collapsed, leading/trailing `-` trimmed, capped at 48
    /// characters), drops empty slugs, joins with `/`, strips any leading
    /// non-alphanumeric character and falls back to `fp` when nothing is left.
    /// The hash covers the parts joined by NUL, so it is exact even where the
    /// slug is lossy. The result always passes [`Fingerprint::parse`].
    ///
    /// **WARNING — pass only semantic, location-independent fields** (engine,
    /// kind/rule, file, scope/symbol). NEVER pass a line number, code snippet,
    /// severity, verdict, model wording or anything else that changes between
    /// runs for the same root cause: doing so makes the fingerprint drift and
    /// silently breaks cross-run matching and de-duplication.
    pub fn derive(parts: &[&str]) -> Fingerprint {
        let mut hasher = Sha256::new();
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                hasher.update([0u8]);
            }
            hasher.update(part.as_bytes());
        }
        let digest = hasher.finalize();
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let hash: String = hex.chars().take(HASH_HEX_CHARS).collect();

        let joined = parts
            .iter()
            .map(|p| slug(p))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("/");
        let prefix: String = joined
            .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
            .chars()
            .take(PREFIX_MAX_CHARS)
            .collect();
        let prefix = if prefix.is_empty() {
            "fp".to_owned()
        } else {
            prefix
        };
        let value = format!("{prefix}@{hash}");
        debug_assert!(
            Fingerprint::parse(&value).is_ok(),
            "derived fingerprint invalid: {value}"
        );
        Fingerprint(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn slug(part: &str) -> String {
    let mut out = String::new();
    for c in part.chars() {
        let mapped = if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-') {
            c
        } else {
            '-'
        };
        if mapped == '-' && out.ends_with('-') {
            continue;
        }
        out.push(mapped);
    }
    // Every char in `out` is ASCII, so taking chars is also byte-safe.
    let capped: String = out
        .trim_matches('-')
        .chars()
        .take(SLUG_PART_MAX_CHARS)
        .collect();
    capped.trim_end_matches('-').to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalRefError {
    /// No refs, an empty ref, or a ref with no visible content.
    Empty,
    SurroundingWhitespace,
    /// `index` is the character (code point) index inside the offending ref.
    ForbiddenCharacter {
        index: usize,
    },
}

impl fmt::Display for CanonicalRefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "canonical reference is empty or has no visible content"),
            Self::SurroundingWhitespace => {
                write!(f, "canonical reference has leading or trailing whitespace")
            }
            Self::ForbiddenCharacter { index } => write!(
                f,
                "canonical reference has a control, format or invisible character at index {index}"
            ),
        }
    }
}

impl std::error::Error for CanonicalRefError {}

/// RFC 3986 unreserved set: everything except `A-Z a-z 0-9 - . _ ~` is encoded.
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Lossless canonical ID: each ref is checked (non-empty, no surrounding
/// whitespace, no `\p{Cc}\p{Cf}\p{Zl}\p{Zp}`/Default_Ignorable code point,
/// some visible content), NFC-normalized, UTF-8 percent-encoded leaving only
/// `A-Za-z0-9-._~` (uppercase `%HH`), and the encoded refs are joined by `::`.
/// Because `:` is always encoded, the separator is unambiguous.
pub fn canonical_id(refs: &[&str]) -> Result<String, CanonicalRefError> {
    if refs.is_empty() {
        return Err(CanonicalRefError::Empty);
    }
    let mut encoded = Vec::with_capacity(refs.len());
    for r in refs {
        if r.is_empty() {
            return Err(CanonicalRefError::Empty);
        }
        if r.trim() != *r {
            return Err(CanonicalRefError::SurroundingWhitespace);
        }
        if let Some(index) = r.chars().position(is_forbidden_identifier_char) {
            return Err(CanonicalRefError::ForbiddenCharacter { index });
        }
        if !has_visible_content(r) {
            return Err(CanonicalRefError::Empty);
        }
        let nfc: String = r.nfc().collect();
        encoded.push(utf8_percent_encode(&nfc, COMPONENT).to_string());
    }
    Ok(encoded.join("::"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_and_rejects() {
        assert!(Fingerprint::parse("semgrep/sqli/src/a.rs@0123456789abcdef").is_ok());
        assert!(Fingerprint::parse("A1._:/@+-").is_ok());
        assert_eq!(Fingerprint::parse("").unwrap_err(), FingerprintError::Empty);
        assert_eq!(
            Fingerprint::parse("-a").unwrap_err(),
            FingerprintError::InvalidLeadingCharacter
        );
        assert_eq!(
            Fingerprint::parse("@a").unwrap_err(),
            FingerprintError::InvalidLeadingCharacter
        );
        assert_eq!(
            Fingerprint::parse("客").unwrap_err(),
            FingerprintError::InvalidLeadingCharacter
        );
        assert_eq!(
            Fingerprint::parse("ab c").unwrap_err(),
            FingerprintError::InvalidCharacter { index: 2 }
        );
        assert_eq!(
            Fingerprint::parse("ab客").unwrap_err(),
            FingerprintError::InvalidCharacter { index: 2 }
        );
        assert_eq!(
            Fingerprint::parse(&"a".repeat(MAX_LEN + 1)).unwrap_err(),
            FingerprintError::TooLong { max: MAX_LEN }
        );
    }

    #[test]
    fn derive_is_stable_and_parses() {
        let a = Fingerprint::derive(&["semgrep", "sql-injection", "src/db.rs"]);
        let b = Fingerprint::derive(&["semgrep", "sql-injection", "src/db.rs"]);
        assert_eq!(a, b);
        assert!(Fingerprint::parse(a.as_str()).is_ok());
        assert!(
            a.as_str().starts_with("semgrep/sql-injection/src/db.rs@"),
            "{a}"
        );
        let hash = a.as_str().rsplit('@').next().unwrap();
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn derive_differs_for_different_parts() {
        let a = Fingerprint::derive(&["semgrep", "sqli", "src/a.rs"]);
        assert_ne!(a, Fingerprint::derive(&["semgrep", "sqli", "src/b.rs"]));
        assert_ne!(a, Fingerprint::derive(&["semgrep", "xss", "src/a.rs"]));
        // Same slug, different raw parts: the hash still separates them.
        let x = Fingerprint::derive(&["a b"]);
        let y = Fingerprint::derive(&["a!b"]);
        assert_ne!(x, y);
        assert_eq!(x.as_str().split('@').next(), y.as_str().split('@').next());
        // Part boundaries matter.
        assert_ne!(
            Fingerprint::derive(&["ab", "c"]),
            Fingerprint::derive(&["a", "bc"])
        );
    }

    #[test]
    fn derive_handles_awkward_input() {
        for parts in [
            &[][..],
            &[""][..],
            &["!!!"][..],
            &[".github/x"][..],
            &["/abs/path"][..],
            &["客戶", "資料"][..],
            &["--a--", "  ", "b"][..],
        ] {
            let fp = Fingerprint::derive(parts);
            assert!(Fingerprint::parse(fp.as_str()).is_ok(), "{parts:?} -> {fp}");
        }
        assert!(Fingerprint::derive(&["!!!"]).as_str().starts_with("fp@"));
        assert!(
            Fingerprint::derive(&[".github/x"])
                .as_str()
                .starts_with("github/x@")
        );
        assert!(
            Fingerprint::derive(&["--a--", "  ", "b"])
                .as_str()
                .starts_with("a/b@")
        );
        let long = "x".repeat(10_000);
        let parts: Vec<&str> = std::iter::repeat_n(long.as_str(), 50).collect();
        let fp = Fingerprint::derive(&parts);
        assert!(Fingerprint::parse(fp.as_str()).is_ok());
        assert!(fp.as_str().len() <= PREFIX_MAX_CHARS + 1 + HASH_HEX_CHARS);
        let first_part = fp.as_str().split('/').next().unwrap();
        assert_eq!(first_part.len(), SLUG_PART_MAX_CHARS);
    }

    #[test]
    fn canonical_id_nfc_and_encoding() {
        let nfc = "caf\u{e9}";
        let nfd = "cafe\u{301}";
        assert_eq!(canonical_id(&[nfc]).unwrap(), canonical_id(&[nfd]).unwrap());
        assert_eq!(canonical_id(&[nfc]).unwrap(), "caf%C3%A9");
        assert_eq!(canonical_id(&["a::b", "c"]).unwrap(), "a%3A%3Ab::c");
        assert_ne!(
            canonical_id(&["a::b", "c"]).unwrap(),
            canonical_id(&["a", "b::c"]).unwrap()
        );
        assert_eq!(canonical_id(&["a b"]).unwrap(), "a%20b");
        assert_eq!(canonical_id(&["A-z_0.9~"]).unwrap(), "A-z_0.9~");
        assert_eq!(canonical_id(&["a/b", "x+y"]).unwrap(), "a%2Fb::x%2By");
        assert_eq!(canonical_id(&["客"]).unwrap(), "%E5%AE%A2");
    }

    #[test]
    fn canonical_id_rejections() {
        assert_eq!(canonical_id(&[]).unwrap_err(), CanonicalRefError::Empty);
        assert_eq!(
            canonical_id(&["a", ""]).unwrap_err(),
            CanonicalRefError::Empty
        );
        assert_eq!(
            canonical_id(&[" a"]).unwrap_err(),
            CanonicalRefError::SurroundingWhitespace
        );
        assert_eq!(
            canonical_id(&["a\n"]).unwrap_err(),
            CanonicalRefError::SurroundingWhitespace
        );
        assert_eq!(
            canonical_id(&["ab\u{200b}c"]).unwrap_err(),
            CanonicalRefError::ForbiddenCharacter { index: 2 }
        );
        assert_eq!(
            canonical_id(&["客\u{0}"]).unwrap_err(),
            CanonicalRefError::ForbiddenCharacter { index: 1 }
        );
        assert_eq!(
            canonical_id(&["a\u{2028}b"]).unwrap_err(),
            CanonicalRefError::ForbiddenCharacter { index: 1 }
        );
    }

    #[test]
    fn display_messages() {
        assert_eq!(FingerprintError::Empty.to_string(), "fingerprint is empty");
        assert!(
            FingerprintError::InvalidLeadingCharacter
                .to_string()
                .contains("start with")
        );
        assert!(
            FingerprintError::InvalidCharacter { index: 3 }
                .to_string()
                .contains("index 3")
        );
        assert!(
            FingerprintError::TooLong { max: 512 }
                .to_string()
                .contains("512")
        );
        assert!(CanonicalRefError::Empty.to_string().contains("empty"));
        assert!(
            CanonicalRefError::SurroundingWhitespace
                .to_string()
                .contains("whitespace")
        );
        assert!(
            CanonicalRefError::ForbiddenCharacter { index: 4 }
                .to_string()
                .contains("index 4")
        );
    }
}
