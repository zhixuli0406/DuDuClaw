//! Strict JSON contract for LLM replies.
//!
//! The whole reply (after trimming and removing at most one outer Markdown
//! fence) must be exactly one JSON value that deserializes into the caller's
//! type. There is no "find the first `{` and the last `}`" slicing: prose
//! around the value, a second value, or a value of the wrong shape all make
//! the reply void. Callers discard a void reply; they never repair it.

use std::fmt;

use serde::de::DeserializeOwned;

/// Why a reply was rejected. Closed set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// Nothing but whitespace (or an empty fence).
    Empty,
    /// The raw reply is larger than the limit.
    TooLarge { bytes: usize, limit: usize },
    /// The reply does not start with a syntactically valid JSON value.
    NotJson { detail: String },
    /// A valid JSON value was followed by more non-whitespace content.
    TrailingContent,
    /// The JSON value does not match the expected schema.
    Schema { detail: String },
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "reply is empty"),
            Self::TooLarge { bytes, limit } => {
                write!(f, "reply is {bytes} bytes, over the {limit}-byte limit")
            }
            Self::NotJson { detail } => write!(f, "reply is not a JSON value: {detail}"),
            Self::TrailingContent => {
                write!(
                    f,
                    "reply has content after the JSON value (exactly one value is allowed)"
                )
            }
            Self::Schema { detail } => {
                write!(f, "reply does not match the expected schema: {detail}")
            }
        }
    }
}

impl std::error::Error for Violation {}

/// Default size limit for a reply: 1 MiB.
pub const DEFAULT_MAX_BYTES: usize = 1 << 20;

/// Parse a reply under [`DEFAULT_MAX_BYTES`]. See [`parse_strict_with_limit`].
pub fn parse_strict<T: DeserializeOwned>(raw: &str) -> Result<T, Violation> {
    parse_strict_with_limit(raw, DEFAULT_MAX_BYTES)
}

/// Parse a reply that must be exactly one JSON value of type `T`.
///
/// Steps: size check on the raw bytes → trim → [`strip_outer_fence`] → the
/// remaining text must be one JSON value followed only by whitespace.
/// Syntax errors are [`Violation::NotJson`], type/shape errors are
/// [`Violation::Schema`] (including duplicate struct fields), anything after
/// the value is [`Violation::TrailingContent`].
pub fn parse_strict_with_limit<T: DeserializeOwned>(
    raw: &str,
    max_bytes: usize,
) -> Result<T, Violation> {
    if raw.len() > max_bytes {
        return Err(Violation::TooLarge {
            bytes: raw.len(),
            limit: max_bytes,
        });
    }
    let body = strip_outer_fence(raw);
    if body.is_empty() {
        return Err(Violation::Empty);
    }
    let mut de = serde_json::Deserializer::from_str(body);
    let value = T::deserialize(&mut de).map_err(|e| match e.classify() {
        serde_json::error::Category::Data => Violation::Schema {
            detail: e.to_string(),
        },
        _ => Violation::NotJson {
            detail: e.to_string(),
        },
    })?;
    de.end().map_err(|_| Violation::TrailingContent)?;
    Ok(value)
}

/// Trim `raw` and remove at most one outer fence pair: an opening line that is
/// exactly ```` ``` ```` or ```` ```json ```` (the `json` tag is ASCII
/// case-insensitive) and a closing ```` ``` ```` at the very end. If both are
/// not present the trimmed input is returned unchanged. The result is trimmed.
pub fn strip_outer_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    let Some(after_open) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let Some(newline) = after_open.find('\n') else {
        return trimmed;
    };
    let (tag, rest) = after_open.split_at(newline);
    let tag = tag.trim();
    if !(tag.is_empty() || tag.eq_ignore_ascii_case("json")) {
        return trimmed;
    }
    match rest.strip_suffix("```") {
        Some(inner) => inner.trim(),
        None => trimmed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct Reply {
        verdict: String,
        score: u32,
    }

    fn ok() -> Reply {
        Reply {
            verdict: "pass".into(),
            score: 3,
        }
    }

    #[test]
    fn accepts_plain_value() {
        let r: Reply = parse_strict(r#"  {"verdict":"pass","score":3}  "#).unwrap();
        assert_eq!(r, ok());
    }

    #[test]
    fn accepts_json_fence() {
        let r: Reply = parse_strict("```json\n{\"verdict\":\"pass\",\"score\":3}\n```").unwrap();
        assert_eq!(r, ok());
        let r: Reply = parse_strict("\n```\n{\"verdict\":\"pass\",\"score\":3}\n```\n").unwrap();
        assert_eq!(r, ok());
        let r: Reply = parse_strict("```JSON\n{\"verdict\":\"pass\",\"score\":3}```").unwrap();
        assert_eq!(r, ok());
    }

    #[test]
    fn rejects_prose_after_json() {
        let e = parse_strict::<Reply>(r#"{"verdict":"pass","score":3} I hope this helps!"#)
            .unwrap_err();
        assert_eq!(e, Violation::TrailingContent);
    }

    #[test]
    fn rejects_prose_before_json() {
        let e = parse_strict::<Reply>(r#"Here you go: {"verdict":"pass","score":3}"#).unwrap_err();
        assert!(matches!(e, Violation::NotJson { .. }), "{e:?}");
    }

    #[test]
    fn rejects_two_values() {
        let e =
            parse_strict::<Reply>(r#"{"verdict":"pass","score":3}{"verdict":"fail","score":1}"#)
                .unwrap_err();
        assert_eq!(e, Violation::TrailingContent);
        let e = parse_strict::<serde_json::Value>("1 2").unwrap_err();
        assert_eq!(e, Violation::TrailingContent);
        let e = parse_strict::<serde_json::Value>("123abc").unwrap_err();
        assert_eq!(e, Violation::TrailingContent);
    }

    #[test]
    fn rejects_prose_only() {
        let e = parse_strict::<Reply>("Sorry, I cannot do that.").unwrap_err();
        assert!(matches!(e, Violation::NotJson { .. }), "{e:?}");
    }

    #[test]
    fn rejects_empty_and_empty_fence() {
        assert_eq!(parse_strict::<Reply>("").unwrap_err(), Violation::Empty);
        assert_eq!(
            parse_strict::<Reply>(" \n\t ").unwrap_err(),
            Violation::Empty
        );
        assert_eq!(
            parse_strict::<Reply>("```json\n```").unwrap_err(),
            Violation::Empty
        );
    }

    #[test]
    fn rejects_over_limit() {
        let raw = r#"{"verdict":"pass","score":3}"#;
        let e = parse_strict_with_limit::<Reply>(raw, 10).unwrap_err();
        assert_eq!(
            e,
            Violation::TooLarge {
                bytes: raw.len(),
                limit: 10
            }
        );
        let big = format!("\"{}\"", "a".repeat(DEFAULT_MAX_BYTES));
        assert!(matches!(
            parse_strict::<String>(&big).unwrap_err(),
            Violation::TooLarge {
                limit: DEFAULT_MAX_BYTES,
                ..
            }
        ));
    }

    #[test]
    fn schema_mismatch_is_schema() {
        let e = parse_strict::<Reply>(r#"{"verdict":"pass"}"#).unwrap_err();
        assert!(matches!(e, Violation::Schema { .. }), "{e:?}");
        let e = parse_strict::<Reply>(r#"{"verdict":"pass","score":3,"extra":1}"#).unwrap_err();
        assert!(matches!(e, Violation::Schema { .. }), "{e:?}");
        let e = parse_strict::<Reply>(r#"{"verdict":"pass","score":3,"score":4}"#).unwrap_err();
        assert!(
            matches!(e, Violation::Schema { .. }),
            "duplicate field: {e:?}"
        );
    }

    #[test]
    fn only_one_fence_layer_is_removed() {
        let raw = "```json\n```json\n{\"verdict\":\"pass\",\"score\":3}\n```\n```";
        assert!(parse_strict::<Reply>(raw).is_err());
    }

    #[test]
    fn unclosed_or_foreign_fence_is_not_stripped() {
        assert_eq!(strip_outer_fence("```json\n{}"), "```json\n{}");
        assert_eq!(
            strip_outer_fence("```python\n{}\n```"),
            "```python\n{}\n```"
        );
        assert_eq!(strip_outer_fence("```{}```"), "```{}```");
        assert_eq!(strip_outer_fence("  {}  "), "{}");
        assert!(parse_strict::<serde_json::Value>("```python\n{}\n```").is_err());
    }

    #[test]
    fn cjk_content_survives() {
        let v: serde_json::Value = parse_strict("```json\n{\"說明\":\"客戶資料\"}\n```").unwrap();
        assert_eq!(v["說明"], "客戶資料");
    }

    #[test]
    fn display_messages() {
        assert_eq!(Violation::Empty.to_string(), "reply is empty");
        assert_eq!(
            Violation::TooLarge {
                bytes: 20,
                limit: 10
            }
            .to_string(),
            "reply is 20 bytes, over the 10-byte limit"
        );
        assert!(
            Violation::NotJson { detail: "x".into() }
                .to_string()
                .contains("not a JSON value: x")
        );
        assert!(
            Violation::TrailingContent
                .to_string()
                .contains("exactly one value")
        );
        assert!(
            Violation::Schema { detail: "y".into() }
                .to_string()
                .contains("schema: y")
        );
    }
}
