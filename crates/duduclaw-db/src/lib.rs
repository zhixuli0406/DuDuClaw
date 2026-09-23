//! Read-only SQL data sources for DuDuClaw (WP-D,
//! `commercial/docs/DESIGN-redaction-field-rules-2026-09.md` §13.7).
//!
//! DuDuClaw's redaction pipeline only sees what passes through its own MCP
//! choke point. A customer who keeps their business data in PostgreSQL / MySQL
//! / SQLite and has no MCP server for it had no way to let an agent read those
//! tables *and* have the rows de-identified on the way out. This crate is the
//! first-party connector that closes that gap: four MCP tools
//! (`db_sources` / `db_tables` / `db_select` / `db_query`) backed by a pool per
//! configured source.
//!
//! # Read-only by construction
//!
//! Three independent layers, in the order they fire:
//!
//! 1. **Statement guard** ([`ensure_read_only_statement`]) — single statement,
//!    must start with `SELECT` / `WITH`. Cheap, first, and explicitly *not*
//!    trusted on its own (see [`guard`]'s module docs).
//! 2. **Driver-level read-only** — SQLite is opened with a read-only file
//!    handle; PostgreSQL and MySQL run every statement inside an explicit
//!    `READ ONLY` transaction *and* set the session's default transaction
//!    characteristics to read-only at connect time.
//! 3. **Caps** — `max_rows` (default 200, hard cap 1000) and `timeout_ms`
//!    (default 10s) bound how much a single call can cost.
//!
//! # Credentials
//!
//! [`DbSourceConfig::url`] is an **already-resolved plaintext** connection
//! string. It is never logged, never serialized, and never included in an
//! error message — [`DbSourceConfig`]'s `Debug` redacts it and
//! [`scrub_connection_details`] strips it (and any `user:pass@host`) out of
//! driver error text before it leaves the crate. Resolution from
//! `config.toml` lives in [`config`], which routes everything through the
//! project's `SecretRef` doctrine.

pub mod config;
pub mod guard;
pub mod ident;
mod source;
mod value;

pub use config::{
    DbSourceEntry, DbSourceLoadError, LoadedDbSources, load_db_sources, open_source,
    parse_db_sources, resolve_source_url,
};
pub use guard::{StatementGuardError, ensure_read_only_statement};
pub use ident::{is_valid_identifier, parse_order_by, placeholder, quote_ident};
pub use source::DbSource;
pub use value::JsonRow;

use serde::{Deserialize, Serialize};

/// Default row cap when a source does not set `max_rows`.
pub const DEFAULT_MAX_ROWS: usize = 200;
/// Hard ceiling on `max_rows`, whatever the config says.
pub const MAX_ROWS_CAP: usize = 1000;
/// Default per-call deadline when a source does not set `timeout_ms`.
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;
/// Floor / ceiling applied to `timeout_ms`.
pub const MIN_TIMEOUT_MS: u64 = 100;
/// Ceiling applied to `timeout_ms` — two minutes is already far past "an agent
/// asked a question about a table".
pub const MAX_TIMEOUT_MS: u64 = 120_000;
/// Wildcard entry accepted in `allowed_tables`.
pub const ALLOW_ALL_TABLES: &str = "*";
/// Most filter clauses accepted on one `db_select`.
pub const MAX_FILTERS: usize = 20;
/// Most `in (...)` members accepted in one filter.
pub const MAX_IN_VALUES: usize = 200;

// ── Driver ──────────────────────────────────────────────────────────────────

/// Which SQL dialect a source speaks.
///
/// A per-driver enum rather than sqlx's `Any` driver: `Any` decodes only the
/// lowest common denominator (no `NUMERIC`, no JSON, no timestamps), which is
/// exactly the §13.7 value mapping this crate has to deliver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Driver {
    Postgres,
    Mysql,
    Sqlite,
}

impl Driver {
    /// Canonical wire string (matches the `config.toml` `driver` key).
    pub fn as_str(self) -> &'static str {
        match self {
            Driver::Postgres => "postgres",
            Driver::Mysql => "mysql",
            Driver::Sqlite => "sqlite",
        }
    }

    /// Parse a `driver` value. Exact match on the canonical name plus the two
    /// spellings people actually type; anything else is refused rather than
    /// guessed (an unknown driver must not silently become the default).
    pub fn parse(s: &str) -> Option<Driver> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("postgres") || s.eq_ignore_ascii_case("postgresql") {
            Some(Driver::Postgres)
        } else if s.eq_ignore_ascii_case("mysql") || s.eq_ignore_ascii_case("mariadb") {
            Some(Driver::Mysql)
        } else if s.eq_ignore_ascii_case("sqlite") || s.eq_ignore_ascii_case("sqlite3") {
            Some(Driver::Sqlite)
        } else {
            None
        }
    }
}

impl std::fmt::Display for Driver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── Config ──────────────────────────────────────────────────────────────────

/// One connectable source. `url` is already-resolved plaintext.
#[derive(Clone)]
pub struct DbSourceConfig {
    pub name: String,
    pub label: String,
    pub driver: Driver,
    /// Already-resolved plaintext connection string. Never logged, never
    /// serialized — see the manual `Debug` below.
    pub url: String,
    pub allowed_tables: Vec<String>,
    pub max_rows: usize,
    pub timeout_ms: u64,
}

impl std::fmt::Debug for DbSourceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbSourceConfig")
            .field("name", &self.name)
            .field("label", &self.label)
            .field("driver", &self.driver)
            .field("url", &"<redacted>")
            .field("allowed_tables", &self.allowed_tables)
            .field("max_rows", &self.max_rows)
            .field("timeout_ms", &self.timeout_ms)
            .finish()
    }
}

impl DbSourceConfig {
    /// Build a config with the §13.7 defaults and caps applied.
    ///
    /// `max_rows` of 0 (or absent) means "use the default", not "no rows" —
    /// matching how the rest of the project reads a missing numeric setting.
    pub fn new(
        name: impl Into<String>,
        driver: Driver,
        url: impl Into<String>,
        allowed_tables: Vec<String>,
    ) -> Self {
        Self {
            name: name.into(),
            label: String::new(),
            driver,
            url: url.into(),
            allowed_tables,
            max_rows: DEFAULT_MAX_ROWS,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn with_max_rows(mut self, max_rows: usize) -> Self {
        self.max_rows = clamp_max_rows(Some(max_rows as i64));
        self
    }

    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = clamp_timeout_ms(Some(timeout_ms as i64));
        self
    }

    /// Is every table visible (`allowed_tables = ["*"]`)?
    ///
    /// Doubles as the free-SQL predicate: `db_query` is permitted only on a
    /// wildcard source (see [`DbError::FreeSqlNotAllowed`]).
    pub fn allows_all_tables(&self) -> bool {
        self.allowed_tables
            .iter()
            .any(|t| t.trim() == ALLOW_ALL_TABLES)
    }

    /// Is `table` reachable through this source?
    ///
    /// Exact, trimmed, ASCII-case-insensitive equality — never a substring
    /// test (project coding convention 2).
    pub fn table_allowed(&self, table: &str) -> bool {
        if self.allows_all_tables() {
            return true;
        }
        let want = table.trim();
        self.allowed_tables
            .iter()
            .any(|t| t.trim().eq_ignore_ascii_case(want))
    }
}

/// Apply the `max_rows` default + cap.
pub fn clamp_max_rows(raw: Option<i64>) -> usize {
    match raw {
        Some(v) if v > 0 => (v as usize).min(MAX_ROWS_CAP),
        _ => DEFAULT_MAX_ROWS,
    }
}

/// Apply the `timeout_ms` default + floor/ceiling.
pub fn clamp_timeout_ms(raw: Option<i64>) -> u64 {
    match raw {
        Some(v) if v > 0 => (v as u64).clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS),
        _ => DEFAULT_TIMEOUT_MS,
    }
}

// ── Query surface ───────────────────────────────────────────────────────────

/// One column of one table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
}

/// One table (or view) the source exposes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableInfo {
    pub name: String,
    pub columns: Vec<ColumnInfo>,
}

/// Comparison operator for a [`Filter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterOp {
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
    Like,
    In,
}

impl FilterOp {
    /// Parse the wire spelling. Both the symbolic (`=`, `!=`, `<=`) and the
    /// word form (`eq`, `ne`, `lte`) are accepted; anything else is refused.
    pub fn parse(s: &str) -> Option<FilterOp> {
        let s = s.trim();
        Some(match s {
            "=" | "==" => FilterOp::Eq,
            "!=" | "<>" => FilterOp::Ne,
            "<" => FilterOp::Lt,
            "<=" => FilterOp::Lte,
            ">" => FilterOp::Gt,
            ">=" => FilterOp::Gte,
            _ if s.eq_ignore_ascii_case("eq") => FilterOp::Eq,
            _ if s.eq_ignore_ascii_case("ne") || s.eq_ignore_ascii_case("neq") => FilterOp::Ne,
            _ if s.eq_ignore_ascii_case("lt") => FilterOp::Lt,
            _ if s.eq_ignore_ascii_case("lte") => FilterOp::Lte,
            _ if s.eq_ignore_ascii_case("gt") => FilterOp::Gt,
            _ if s.eq_ignore_ascii_case("gte") => FilterOp::Gte,
            _ if s.eq_ignore_ascii_case("like") => FilterOp::Like,
            _ if s.eq_ignore_ascii_case("in") => FilterOp::In,
            _ => return None,
        })
    }

    /// The SQL operator token. Fixed strings only — never operator-supplied
    /// text.
    pub fn sql(self) -> &'static str {
        match self {
            FilterOp::Eq => "=",
            FilterOp::Ne => "<>",
            FilterOp::Lt => "<",
            FilterOp::Lte => "<=",
            FilterOp::Gt => ">",
            FilterOp::Gte => ">=",
            FilterOp::Like => "LIKE",
            FilterOp::In => "IN",
        }
    }
}

/// One `WHERE` clause term.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    pub column: String,
    pub op: FilterOp,
    pub value: serde_json::Value,
}

/// A structured `SELECT`. Everything here is validated before it reaches SQL:
/// identifiers against [`is_valid_identifier`], values as bound parameters.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SelectRequest {
    pub table: String,
    /// Empty ⇒ `SELECT *`.
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default)]
    pub filter: Vec<Filter>,
    #[serde(default)]
    pub order_by: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Rows plus the two facts a caller needs to know about them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QueryResult {
    pub rows: Vec<JsonRow>,
    pub row_count: usize,
    /// `true` when the source had more rows than `max_rows` allowed back.
    pub truncated: bool,
}

// ── Errors ──────────────────────────────────────────────────────────────────

/// Everything this crate can fail with. No variant ever carries the connection
/// URL — see [`scrub_connection_details`].
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("資料來源「{0}」不存在或未設定")]
    UnknownSource(String),

    // NOTE: the field is `source_name`, not `source` — thiserror treats a
    // field literally named `source` as the error's `std::error::Error::source`
    // and would try to require `String: Error`.
    #[error("資料來源「{source_name}」不允許存取資料表「{table}」（請檢查 allowed_tables）")]
    TableNotAllowed { source_name: String, table: String },

    /// Free-form SQL on a source that has a real table allowlist.
    ///
    /// `db_query` cannot be bounded by `allowed_tables` — enumerating the
    /// tables an arbitrary statement touches needs a real SQL parser, and a
    /// half-working one would reject legitimate CTEs while still missing
    /// cases. So the rule is fail-closed at the source level instead: an
    /// allowlist means "these tables, via `db_select`"; free SQL requires the
    /// operator to have said `["*"]` out loud.
    #[error(
        "資料來源「{source_name}」設有資料表白名單，只能用 db_select 讀取白名單內的資料表；要開放自由 SQL 請將 allowed_tables 設為 [\"*\"]"
    )]
    FreeSqlNotAllowed { source_name: String },

    #[error("不合法的識別字「{0}」（僅允許英數字與底線，且不可數字開頭）")]
    InvalidIdentifier(String),

    #[error("不合法的查詢條件：{0}")]
    InvalidFilter(String),

    #[error("{0}")]
    StatementRejected(#[from] StatementGuardError),

    #[error("資料庫連線失敗：{0}")]
    Connect(String),

    #[error("查詢失敗：{0}")]
    Query(String),

    #[error("查詢逾時（{0} ms）")]
    Timeout(u64),

    #[error("設定錯誤：{0}")]
    Config(String),
}

/// Remove anything credential-shaped from driver error text.
///
/// Two passes: the exact connection string (when the caller can supply it),
/// then any `scheme://user:pass@host` authority that a driver may have echoed
/// from a *different* string. Every index used for slicing comes from `find` /
/// `rfind` on an ASCII needle, so it is always on a char boundary — CJK error
/// text passes through untouched (project coding convention 1).
pub fn scrub_connection_details(message: &str, url: Option<&str>) -> String {
    let mut out = message.to_string();
    if let Some(url) = url
        && !url.is_empty()
    {
        out = out.replace(url, "<redacted-url>");
    }
    // Strip `user:pass@` out of any remaining URL-ish text.
    let mut cleaned = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(pos) = rest.find("://") {
        let (head, tail) = rest.split_at(pos + 3);
        cleaned.push_str(head);
        // Authority runs to the first `/`, whitespace, or end.
        let auth_end = tail
            .find(|c: char| c == '/' || c.is_whitespace())
            .unwrap_or(tail.len());
        let (authority, remainder) = tail.split_at(auth_end);
        match authority.rfind('@') {
            Some(at) => {
                cleaned.push_str("<redacted>@");
                cleaned.push_str(&authority[at + 1..]);
            }
            None => cleaned.push_str(authority),
        }
        rest = remainder;
    }
    cleaned.push_str(rest);
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_parses_known_spellings_and_refuses_others() {
        assert_eq!(Driver::parse("postgres"), Some(Driver::Postgres));
        assert_eq!(Driver::parse("PostgreSQL"), Some(Driver::Postgres));
        assert_eq!(Driver::parse(" mysql "), Some(Driver::Mysql));
        assert_eq!(Driver::parse("mariadb"), Some(Driver::Mysql));
        assert_eq!(Driver::parse("sqlite3"), Some(Driver::Sqlite));
        assert_eq!(Driver::parse("oracle"), None);
        assert_eq!(Driver::parse(""), None);
    }

    #[test]
    fn max_rows_defaults_and_caps() {
        assert_eq!(clamp_max_rows(None), DEFAULT_MAX_ROWS);
        assert_eq!(clamp_max_rows(Some(0)), DEFAULT_MAX_ROWS);
        assert_eq!(clamp_max_rows(Some(-5)), DEFAULT_MAX_ROWS);
        assert_eq!(clamp_max_rows(Some(50)), 50);
        assert_eq!(clamp_max_rows(Some(999_999)), MAX_ROWS_CAP);
    }

    #[test]
    fn timeout_defaults_and_clamps() {
        assert_eq!(clamp_timeout_ms(None), DEFAULT_TIMEOUT_MS);
        assert_eq!(clamp_timeout_ms(Some(0)), DEFAULT_TIMEOUT_MS);
        assert_eq!(clamp_timeout_ms(Some(5)), MIN_TIMEOUT_MS);
        assert_eq!(clamp_timeout_ms(Some(2_000)), 2_000);
        assert_eq!(clamp_timeout_ms(Some(9_999_999)), MAX_TIMEOUT_MS);
    }

    #[test]
    fn table_allowlist_is_exact_not_substring() {
        let cfg = DbSourceConfig::new(
            "s",
            Driver::Sqlite,
            "/tmp/x.sqlite",
            vec!["customers".into(), "orders".into()],
        );
        assert!(cfg.table_allowed("customers"));
        assert!(cfg.table_allowed(" Customers "));
        // Substring matches must NOT pass.
        assert!(!cfg.table_allowed("customer"));
        assert!(!cfg.table_allowed("customers_secret"));
        assert!(!cfg.table_allowed("secret_customers"));
    }

    #[test]
    fn wildcard_allows_everything() {
        let cfg = DbSourceConfig::new("s", Driver::Sqlite, "/tmp/x.sqlite", vec!["*".into()]);
        assert!(cfg.allows_all_tables());
        assert!(cfg.table_allowed("anything_at_all"));
    }

    #[test]
    fn debug_never_prints_the_url() {
        let cfg = DbSourceConfig::new(
            "s",
            Driver::Postgres,
            "postgres://user:hunter2@db.internal/app",
            vec!["*".into()],
        );
        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(!rendered.contains("db.internal"), "{rendered}");
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn scrub_removes_url_and_credentials() {
        let url = "postgres://user:hunter2@db.internal:5432/app";
        let msg = format!("failed to connect to {url}: timed out");
        let out = scrub_connection_details(&msg, Some(url));
        assert!(!out.contains("hunter2"), "{out}");
        assert!(!out.contains("db.internal"), "{out}");

        // Even without the exact url, embedded credentials are stripped.
        let out2 = scrub_connection_details(
            "error at mysql://root:pw@10.0.0.4/shop while reading",
            None,
        );
        assert!(!out2.contains("root:pw"), "{out2}");
        assert!(out2.contains("<redacted>@10.0.0.4/shop"), "{out2}");
    }

    #[test]
    fn scrub_is_safe_on_cjk_and_plain_text() {
        let msg = "資料表「客戶」不存在";
        assert_eq!(scrub_connection_details(msg, None), msg);
        assert_eq!(scrub_connection_details(msg, Some("")), msg);
    }

    #[test]
    fn filter_ops_parse_both_spellings() {
        assert_eq!(FilterOp::parse("="), Some(FilterOp::Eq));
        assert_eq!(FilterOp::parse("eq"), Some(FilterOp::Eq));
        assert_eq!(FilterOp::parse("!="), Some(FilterOp::Ne));
        assert_eq!(FilterOp::parse("LIKE"), Some(FilterOp::Like));
        assert_eq!(FilterOp::parse("in"), Some(FilterOp::In));
        assert_eq!(FilterOp::parse("; DROP"), None);
        assert_eq!(FilterOp::parse("regexp"), None);
    }
}
