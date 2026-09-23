//! SQL identifier validation and per-driver quoting.
//!
//! Every identifier that reaches generated SQL (`db_select`'s table, column,
//! and `ORDER BY` names) passes [`is_valid_identifier`] first — the §13.7
//! contract's `^[A-Za-z_][A-Za-z0-9_]*$`. Values never travel this path; they
//! are always bound as parameters.
//!
//! Validation is deliberately the *only* defence that matters here: once a
//! string is known to be `[A-Za-z_][A-Za-z0-9_]*` it cannot close a quote, so
//! [`quote_ident`] is a formatting convenience (it protects reserved words and
//! case-sensitivity), not the security boundary.

use crate::Driver;

/// Longest identifier accepted. PostgreSQL truncates at 63 bytes and MySQL at
/// 64; anything past that is either a typo or an attempt to blow up an error
/// message, so it is refused rather than silently truncated.
pub const MAX_IDENT_LEN: usize = 64;

/// `^[A-Za-z_][A-Za-z0-9_]*$`, capped at [`MAX_IDENT_LEN`].
///
/// Written as an explicit scan instead of a regex so the crate carries no
/// regex dependency and so the rule is readable at the call site.
pub fn is_valid_identifier(s: &str) -> bool {
    if s.is_empty() || s.len() > MAX_IDENT_LEN {
        return false;
    }
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Quote a **already-validated** identifier for `driver`.
///
/// # Panics
/// Never — but passing an unvalidated string is a bug: call
/// [`is_valid_identifier`] first. Debug builds assert it.
pub fn quote_ident(driver: Driver, s: &str) -> String {
    debug_assert!(
        is_valid_identifier(s),
        "quote_ident called with an unvalidated identifier"
    );
    match driver {
        Driver::Mysql => format!("`{s}`"),
        Driver::Postgres | Driver::Sqlite => format!("\"{s}\""),
    }
}

/// Bind-parameter placeholder for the `idx`-th (1-based) parameter.
pub fn placeholder(driver: Driver, idx: usize) -> String {
    match driver {
        Driver::Postgres => format!("${idx}"),
        Driver::Mysql | Driver::Sqlite => "?".to_string(),
    }
}

/// Parse an `order_by` clause of the shape `column`, `column asc`, or
/// `column desc` into its validated identifier plus a canonical direction.
///
/// Returns `None` when the column fails [`is_valid_identifier`] or the
/// direction word is anything other than `asc`/`desc` — fail-closed, because
/// the alternative is interpolating operator-supplied text into SQL.
pub fn parse_order_by(raw: &str) -> Option<(String, &'static str)> {
    let mut parts = raw.split_whitespace();
    let column = parts.next()?;
    if !is_valid_identifier(column) {
        return None;
    }
    let direction = match parts.next() {
        None => "ASC",
        Some(d) if d.eq_ignore_ascii_case("asc") => "ASC",
        Some(d) if d.eq_ignore_ascii_case("desc") => "DESC",
        Some(_) => return None,
    };
    // Anything after the direction word is unexpected input, not a clause we
    // understand — refuse rather than ignore it.
    if parts.next().is_some() {
        return None;
    }
    Some((column.to_string(), direction))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_identifiers() {
        for s in ["a", "_x", "customers", "Order_2", "_", "A1_b2"] {
            assert!(is_valid_identifier(s), "should accept {s}");
        }
    }

    #[test]
    fn rejects_injection_shapes() {
        for s in [
            "",
            "1abc",
            "a-b",
            "a b",
            "a;b",
            "a'b",
            "a\"b",
            "a`b",
            "customers; DROP TABLE t",
            "客戶",
            "a.b",
            "*",
        ] {
            assert!(!is_valid_identifier(s), "should reject {s:?}");
        }
    }

    #[test]
    fn rejects_overlong_identifier() {
        let long = "a".repeat(MAX_IDENT_LEN + 1);
        assert!(!is_valid_identifier(&long));
        assert!(is_valid_identifier(&"a".repeat(MAX_IDENT_LEN)));
    }

    #[test]
    fn quotes_per_driver() {
        assert_eq!(quote_ident(Driver::Postgres, "t"), "\"t\"");
        assert_eq!(quote_ident(Driver::Sqlite, "t"), "\"t\"");
        assert_eq!(quote_ident(Driver::Mysql, "t"), "`t`");
    }

    #[test]
    fn placeholders_per_driver() {
        assert_eq!(placeholder(Driver::Postgres, 3), "$3");
        assert_eq!(placeholder(Driver::Mysql, 3), "?");
        assert_eq!(placeholder(Driver::Sqlite, 1), "?");
    }

    #[test]
    fn order_by_parses_and_rejects() {
        assert_eq!(
            parse_order_by("name"),
            Some(("name".to_string(), "ASC"))
        );
        assert_eq!(
            parse_order_by("name desc"),
            Some(("name".to_string(), "DESC"))
        );
        assert_eq!(
            parse_order_by("name  ASC"),
            Some(("name".to_string(), "ASC"))
        );
        assert_eq!(parse_order_by("name; DROP TABLE t"), None);
        assert_eq!(parse_order_by("name sideways"), None);
        assert_eq!(parse_order_by("name asc, id desc"), None);
        assert_eq!(parse_order_by(""), None);
    }
}
