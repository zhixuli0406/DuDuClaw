//! Layer (a) of the §13.7 read-only triple guard: the *statement* guard.
//!
//! This is the cheapest and least trustworthy of the three layers, and it is
//! deliberately written that way. It refuses anything that is not obviously a
//! single read statement; the two layers behind it —
//! a driver-level READ ONLY transaction (PostgreSQL / MySQL) or a read-only
//! file handle (SQLite), and the row/time caps — are what actually make a
//! write impossible. A data-modifying CTE
//! (`WITH x AS (DELETE … RETURNING *) SELECT * FROM x`) passes *this* layer on
//! purpose: it is a single statement starting with `WITH`. The transaction
//! layer is what rejects it. Never weaken layer (b) on the assumption that
//! this one is a parser.
//!
//! Escaping dialects differ, so the scanner picks the **fail-closed** reading
//! every time. Backslash is NOT treated as a string escape (PostgreSQL's
//! `standard_conforming_strings` semantics). Under MySQL's backslash-escape
//! dialect that makes `'it\'s; fine'` scan as a closed string followed by a
//! stray `;`, which is rejected — a false refusal, never a false accept. Use
//! `''` doubling, which both dialects accept.

/// Longest free-form statement accepted. A read query longer than this is
/// pathological input, not a question about business data.
pub const MAX_SQL_LEN: usize = 20_000;

/// Why a statement was refused. Messages are user-facing (zh-TW) at the MCP
/// boundary; the variants stay in English for matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatementGuardError {
    /// Nothing but whitespace/comments.
    Empty,
    /// Longer than [`MAX_SQL_LEN`].
    TooLong { len: usize },
    /// Does not begin with `SELECT` / `WITH`.
    NotReadOnly { first_token: String },
    /// A `;` outside a string literal / quoted identifier / comment.
    MultipleStatements,
    /// A string literal, quoted identifier, or block comment never closed.
    UnterminatedLiteral,
}

impl std::fmt::Display for StatementGuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "SQL 語句是空的（去除註解與空白後沒有內容）"),
            Self::TooLong { len } => write!(
                f,
                "SQL 語句過長（{len} 字元，上限 {MAX_SQL_LEN}）"
            ),
            Self::NotReadOnly { first_token } => write!(
                f,
                "只允許唯讀查詢：語句必須以 SELECT 或 WITH 開頭（實際開頭是「{first_token}」）"
            ),
            Self::MultipleStatements => write!(
                f,
                "只允許單一語句：字串字面值以外不得出現分號（連結尾的分號也請移除）"
            ),
            Self::UnterminatedLiteral => write!(
                f,
                "SQL 語句有未閉合的字串、識別字引號或區塊註解"
            ),
        }
    }
}

impl std::error::Error for StatementGuardError {}

/// Layer (a): accept only a single `SELECT` / `WITH` statement.
pub fn ensure_read_only_statement(sql: &str) -> Result<(), StatementGuardError> {
    let len = sql.chars().count();
    if len > MAX_SQL_LEN {
        return Err(StatementGuardError::TooLong { len });
    }
    // One pass does both jobs: it records the first token found outside any
    // comment, and it flags a `;` found outside any literal.
    let scan = scan(sql)?;
    let Some(first_token) = scan.first_token else {
        return Err(StatementGuardError::Empty);
    };
    if scan.saw_semicolon {
        return Err(StatementGuardError::MultipleStatements);
    }
    let upper = first_token.to_ascii_uppercase();
    if upper != "SELECT" && upper != "WITH" {
        return Err(StatementGuardError::NotReadOnly { first_token });
    }
    Ok(())
}

struct Scan {
    first_token: Option<String>,
    saw_semicolon: bool,
}

/// Single-pass lexer over the statement.
///
/// Char-based throughout (never a byte index into the string) so multi-byte
/// input — a CJK literal in a `WHERE` clause — cannot panic or be mis-split.
fn scan(sql: &str) -> Result<Scan, StatementGuardError> {
    let chars: Vec<char> = sql.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    let mut first_token: Option<String> = None;
    let mut saw_semicolon = false;

    while i < n {
        let c = chars[i];

        // ── comments ────────────────────────────────────────────────────
        if c == '-' && i + 1 < n && chars[i + 1] == '-' {
            i += 2;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            // PostgreSQL nests block comments; the standard does not. Counting
            // depth is the safer reading: it can only make us consume *more*
            // as comment, and an unterminated comment is rejected outright.
            let mut depth = 1usize;
            i += 2;
            while i < n && depth > 0 {
                if chars[i] == '/' && i + 1 < n && chars[i + 1] == '*' {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && i + 1 < n && chars[i + 1] == '/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if depth > 0 {
                return Err(StatementGuardError::UnterminatedLiteral);
            }
            continue;
        }

        // ── quoted regions ──────────────────────────────────────────────
        if c == '\'' || c == '"' || c == '`' {
            let quote = c;
            i += 1;
            loop {
                if i >= n {
                    return Err(StatementGuardError::UnterminatedLiteral);
                }
                if chars[i] == quote {
                    // Doubled quote = an escaped quote, stay inside.
                    if i + 1 < n && chars[i + 1] == quote {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }

        if c == ';' {
            saw_semicolon = true;
            i += 1;
            continue;
        }

        // ── first bare word ─────────────────────────────────────────────
        if first_token.is_none() && (c.is_ascii_alphabetic() || c == '_') {
            let start = i;
            while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            first_token = Some(chars[start..i].iter().collect());
            continue;
        }
        // A statement whose first non-comment character is not a word (e.g. a
        // leading `(` around a SELECT) records that character so the error
        // message is honest about what was found.
        if first_token.is_none() && !c.is_whitespace() {
            first_token = Some(c.to_string());
            i += 1;
            continue;
        }

        i += 1;
    }

    Ok(Scan {
        first_token,
        saw_semicolon,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(sql: &str) {
        assert!(
            ensure_read_only_statement(sql).is_ok(),
            "should accept: {sql:?} → {:?}",
            ensure_read_only_statement(sql)
        );
    }
    fn err(sql: &str) -> StatementGuardError {
        ensure_read_only_statement(sql).expect_err(&format!("should reject: {sql:?}"))
    }

    #[test]
    fn accepts_plain_select() {
        ok("SELECT * FROM customers");
        ok("  select id, name from customers where name = 'a;b' ");
        ok("SeLeCt 1");
    }

    #[test]
    fn accepts_with_select() {
        ok("WITH recent AS (SELECT * FROM orders) SELECT * FROM recent");
        ok("-- a comment\nWITH x AS (SELECT 1) SELECT * FROM x");
    }

    #[test]
    fn strips_leading_comments_before_prefix_check() {
        ok("/* hello */ SELECT 1");
        ok("/* a /* nested */ still comment */ SELECT 1");
        ok("--x\n--y\nSELECT 1");
    }

    #[test]
    fn rejects_comment_prefixed_delete() {
        assert_eq!(
            err("--x\nDELETE FROM customers"),
            StatementGuardError::NotReadOnly {
                first_token: "DELETE".to_string()
            }
        );
        assert_eq!(
            err("/* nice */ UPDATE customers SET name = 'x'"),
            StatementGuardError::NotReadOnly {
                first_token: "UPDATE".to_string()
            }
        );
    }

    #[test]
    fn rejects_writes() {
        for sql in [
            "DELETE FROM customers",
            "UPDATE customers SET name='x'",
            "INSERT INTO customers VALUES (1)",
            "DROP TABLE customers",
            "TRUNCATE customers",
            "ALTER TABLE customers ADD COLUMN x INT",
            "CREATE TABLE t (a INT)",
            "PRAGMA writable_schema = ON",
            "ATTACH DATABASE 'x' AS y",
            "COPY customers FROM '/etc/passwd'",
        ] {
            assert!(
                matches!(err(sql), StatementGuardError::NotReadOnly { .. }),
                "expected NotReadOnly for {sql:?}"
            );
        }
    }

    #[test]
    fn rejects_multi_statement() {
        assert_eq!(
            err("SELECT 1; DELETE FROM customers"),
            StatementGuardError::MultipleStatements
        );
        // Even a lone trailing semicolon is refused — the contract says no `;`
        // outside a literal, and "just the last one" is exactly the carve-out
        // an attacker aims at.
        assert_eq!(
            err("SELECT 1;"),
            StatementGuardError::MultipleStatements
        );
    }

    #[test]
    fn semicolon_inside_literal_is_fine() {
        ok("SELECT * FROM t WHERE note = 'a;b'");
        ok("SELECT * FROM t WHERE note = 'it''s; fine'");
        ok("SELECT \"weird;col\" FROM t");
        ok("SELECT `weird;col` FROM t");
    }

    #[test]
    fn rejects_unterminated() {
        assert_eq!(
            err("SELECT * FROM t WHERE a = 'unclosed"),
            StatementGuardError::UnterminatedLiteral
        );
        assert_eq!(
            err("/* unclosed SELECT 1"),
            StatementGuardError::UnterminatedLiteral
        );
    }

    #[test]
    fn rejects_empty_and_comment_only() {
        assert_eq!(err("   "), StatementGuardError::Empty);
        assert_eq!(err("-- nothing here"), StatementGuardError::Empty);
        assert_eq!(err("/* nothing */"), StatementGuardError::Empty);
    }

    #[test]
    fn rejects_overlong() {
        let sql = format!("SELECT '{}'", "a".repeat(MAX_SQL_LEN));
        assert!(matches!(
            ensure_read_only_statement(&sql),
            Err(StatementGuardError::TooLong { .. })
        ));
    }

    #[test]
    fn cjk_literal_does_not_panic_and_is_accepted() {
        ok("SELECT * FROM 客 WHERE name = '王小明；測試'");
    }

    #[test]
    fn select_prefix_must_be_a_whole_word() {
        // `SELECTX` is not `SELECT` — word-bounded token extraction, not a
        // `starts_with` prefix test (project convention 2).
        assert_eq!(
            err("SELECTX 1"),
            StatementGuardError::NotReadOnly {
                first_token: "SELECTX".to_string()
            }
        );
        assert_eq!(
            err("WITHOUT ROWID"),
            StatementGuardError::NotReadOnly {
                first_token: "WITHOUT".to_string()
            }
        );
    }

    #[test]
    fn data_modifying_cte_passes_layer_a_by_design() {
        // Documented above: layer (b) — the READ ONLY transaction / read-only
        // file handle — is what rejects this. Locking the behaviour here so a
        // future edit cannot quietly assume layer (a) catches it.
        ok("WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x");
    }
}
