//! The connected source: pools, schema listing, structured select, free-form
//! read-only query.

use std::collections::{BTreeMap, HashSet};
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use futures_util::TryStreamExt;
use sqlx::{Executor, Row};
use sqlx::mysql::{MySqlPool, MySqlPoolOptions, MySqlRow};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow};

use crate::value::{JsonRow, mysql_row_to_json, pg_row_to_json, sqlite_row_to_json};
use crate::{
    ColumnInfo, DbError, DbSourceConfig, Driver, Filter, FilterOp, MAX_FILTERS, MAX_IN_VALUES,
    QueryResult, SelectRequest, TableInfo, ensure_read_only_statement, ident,
    scrub_connection_details,
};

/// Most tables one `db_tables` answer may describe. A schema with more than
/// this is a data warehouse, not something to page into an agent prompt.
const MAX_TABLES: usize = 500;

/// Small on purpose: an agent asking questions about a table does not need a
/// connection pool, and a runaway loop should not be able to exhaust the
/// customer's database connections.
const POOL_MAX_CONNECTIONS: u32 = 2;

/// A parameter value bound into generated SQL. Values never reach the SQL
/// string itself.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Bind {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

/// Generated SQL plus its ordered bind values.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BuiltSelect {
    pub sql: String,
    pub binds: Vec<Bind>,
}

enum Pool {
    Postgres(PgPool),
    MySql(MySqlPool),
    Sqlite(SqlitePool),
}

/// A live, read-only handle on one configured data source.
pub struct DbSource {
    cfg: DbSourceConfig,
    pool: Pool,
}

impl std::fmt::Debug for DbSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbSource").field("cfg", &self.cfg).finish()
    }
}

/// Sources already warned about `allowed_tables = ["*"]`, so the warning is
/// emitted once per process per source rather than on every connect.
fn wildcard_warned() -> &'static Mutex<HashSet<String>> {
    static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    WARNED.get_or_init(|| Mutex::new(HashSet::new()))
}

impl DbSource {
    /// Open a pool for `cfg`.
    ///
    /// SQLite is opened with a read-only file handle. PostgreSQL and MySQL
    /// get their read-only guarantee per statement instead, from the explicit
    /// `READ ONLY` transaction [`DbSource::fetch`] opens.
    ///
    /// A pool-level `after_connect` hook setting the *session* default
    /// transaction characteristics was tried and removed: sqlx 0.8's
    /// `after_connect` closure signature is not general enough over
    /// connection lifetimes, which makes every future that transitively
    /// contains `connect()` non-`Send` — and the MCP HTTP/SSE transports hand
    /// those futures to axum, which requires `Send`. The belt was not worth
    /// losing the transport.
    pub async fn connect(cfg: DbSourceConfig) -> Result<DbSource, DbError> {
        if cfg.allows_all_tables()
            && let Ok(mut seen) = wildcard_warned().lock()
            && seen.insert(cfg.name.clone())
        {
            tracing::warn!(
                source = %cfg.name,
                driver = %cfg.driver,
                "db source allows every table (allowed_tables = [\"*\"]) — agents granted \
                 this source can read any table in the default schema"
            );
        }

        let acquire = Duration::from_millis(cfg.timeout_ms);
        let pool = match cfg.driver {
            Driver::Postgres => {
                let p = PgPoolOptions::new()
                    .max_connections(POOL_MAX_CONNECTIONS)
                    .acquire_timeout(acquire)
                    .connect(&cfg.url)
                    .await
                    .map_err(|e| connect_err(&e, &cfg))?;
                Pool::Postgres(p)
            }
            Driver::Mysql => {
                let p = MySqlPoolOptions::new()
                    .max_connections(POOL_MAX_CONNECTIONS)
                    .acquire_timeout(acquire)
                    .connect(&cfg.url)
                    .await
                    .map_err(|e| connect_err(&e, &cfg))?;
                Pool::MySql(p)
            }
            Driver::Sqlite => {
                let opts = sqlite_options(&cfg.url)?;
                let p = SqlitePoolOptions::new()
                    .max_connections(POOL_MAX_CONNECTIONS)
                    .acquire_timeout(acquire)
                    .connect_with(opts)
                    .await
                    .map_err(|e| connect_err(&e, &cfg))?;
                Pool::Sqlite(p)
            }
        };
        Ok(DbSource { cfg, pool })
    }

    pub fn config(&self) -> &DbSourceConfig {
        &self.cfg
    }

    /// Cheapest possible liveness probe — used by `db_sources.test`.
    pub async fn ping(&self) -> Result<(), DbError> {
        self.run(async {
            match &self.pool {
                Pool::Postgres(p) => {
                    sqlx::query("SELECT 1").fetch_one(p).await.map(|_| ())
                }
                Pool::MySql(p) => sqlx::query("SELECT 1").fetch_one(p).await.map(|_| ()),
                Pool::Sqlite(p) => sqlx::query("SELECT 1").fetch_one(p).await.map(|_| ()),
            }
        })
        .await?
        .map_err(|e| DbError::Query(self.scrub(&e.to_string())))
    }

    /// Tables (and views) in the default schema, filtered by `allowed_tables`.
    pub async fn list_tables(&self) -> Result<Vec<TableInfo>, DbError> {
        let tables = self
            .run(self.list_tables_inner())
            .await?
            .map_err(|e| DbError::Query(self.scrub(&e.to_string())))?;
        Ok(tables)
    }

    async fn list_tables_inner(&self) -> Result<Vec<TableInfo>, sqlx::Error> {
        match &self.pool {
            Pool::Postgres(p) => {
                let rows = sqlx::query(PG_SCHEMA_SQL).fetch_all(p).await?;
                let mut triples = Vec::with_capacity(rows.len());
                for r in &rows {
                    triples.push((
                        pg_schema_text(r, "table_name")?,
                        pg_schema_text(r, "column_name")?,
                        pg_schema_text(r, "data_type")?,
                    ));
                }
                Ok(self.group_schema_rows(triples.into_iter()))
            }
            Pool::MySql(p) => {
                let rows = sqlx::query(MYSQL_SCHEMA_SQL).fetch_all(p).await?;
                let mut triples = Vec::with_capacity(rows.len());
                for r in &rows {
                    triples.push((
                        mysql_schema_text(r, "table_name")?,
                        mysql_schema_text(r, "column_name")?,
                        mysql_schema_text(r, "data_type")?,
                    ));
                }
                Ok(self.group_schema_rows(triples.into_iter()))
            }
            Pool::Sqlite(p) => {
                // SQLite has no information_schema. Names come from
                // sqlite_master; columns from the pragma table-valued
                // function, queried only for tables the allowlist admits.
                let rows = sqlx::query(SQLITE_TABLES_SQL).fetch_all(p).await?;
                let mut out = Vec::new();
                for row in rows {
                    let name = sqlite_schema_text(&row, "name")?;
                    if name.is_empty() || !self.cfg.table_allowed(&name) {
                        continue;
                    }
                    if out.len() >= MAX_TABLES {
                        break;
                    }
                    let cols = sqlx::query("SELECT name, type FROM pragma_table_info(?)")
                        .bind(name.clone())
                        .fetch_all(p)
                        .await?;
                    let mut columns = Vec::with_capacity(cols.len());
                    for c in &cols {
                        columns.push(ColumnInfo {
                            name: sqlite_schema_text(c, "name")?,
                            data_type: sqlite_schema_text(c, "type")?,
                        });
                    }
                    out.push(TableInfo { name, columns });
                }
                Ok(out)
            }
        }
    }

    fn group_schema_rows<I>(&self, rows: I) -> Vec<TableInfo>
    where
        I: Iterator<Item = (String, String, String)>,
    {
        let mut grouped: BTreeMap<String, Vec<ColumnInfo>> = BTreeMap::new();
        for (table, column, data_type) in rows {
            if table.is_empty() || !self.cfg.table_allowed(&table) {
                continue;
            }
            if !grouped.contains_key(&table) && grouped.len() >= MAX_TABLES {
                continue;
            }
            grouped.entry(table).or_default().push(ColumnInfo {
                name: column,
                data_type,
            });
        }
        grouped
            .into_iter()
            .map(|(name, columns)| TableInfo { name, columns })
            .collect()
    }

    /// Structured `SELECT`. Identifiers are validated and quoted; every value
    /// is a bound parameter; the table must be in `allowed_tables`.
    pub async fn select(&self, req: &SelectRequest) -> Result<QueryResult, DbError> {
        let cap = self.effective_limit(req.limit);
        let built = build_select(&self.cfg, req, cap)?;
        self.run(self.fetch(&built.sql, &built.binds, cap))
            .await?
            .map_err(|e| DbError::Query(self.scrub(&e.to_string())))
    }

    /// Free-form read-only SQL.
    ///
    /// **Refused outright when the source has a real table allowlist.** The
    /// allowlist cannot be enforced against an arbitrary statement without a
    /// real SQL parser, so rather than let free SQL quietly read past it, a
    /// non-wildcard source answers [`DbError::FreeSqlNotAllowed`] — checked
    /// first, before the statement guard and before any connection is used.
    /// `allowed_tables = ["*"]` is the operator saying free SQL is fine here.
    pub async fn query(&self, sql: &str, limit: Option<usize>) -> Result<QueryResult, DbError> {
        if !self.cfg.allows_all_tables() {
            return Err(DbError::FreeSqlNotAllowed {
                source_name: self.cfg.name.clone(),
            });
        }
        ensure_read_only_statement(sql)?;
        let cap = self.effective_limit(limit);
        self.run(self.fetch(sql, &[], cap))
            .await?
            .map_err(|e| DbError::Query(self.scrub(&e.to_string())))
    }

    /// Close the underlying pool. Idempotent.
    pub async fn close(&self) {
        match &self.pool {
            Pool::Postgres(p) => p.close().await,
            Pool::MySql(p) => p.close().await,
            Pool::Sqlite(p) => p.close().await,
        }
    }

    fn effective_limit(&self, requested: Option<usize>) -> usize {
        match requested {
            Some(v) if v > 0 => v.min(self.cfg.max_rows),
            _ => self.cfg.max_rows,
        }
    }

    fn scrub(&self, message: &str) -> String {
        scrub_connection_details(message, Some(&self.cfg.url))
    }

    /// Layer (c): wrap any database work in the source's deadline.
    async fn run<F, T>(&self, fut: F) -> Result<T, DbError>
    where
        F: std::future::Future<Output = T>,
    {
        tokio::time::timeout(Duration::from_millis(self.cfg.timeout_ms), fut)
            .await
            .map_err(|_| DbError::Timeout(self.cfg.timeout_ms))
    }

    /// Layer (b): run `sql` inside a driver-appropriate read-only context and
    /// stream at most `cap` rows back.
    async fn fetch(
        &self,
        sql: &str,
        binds: &[Bind],
        cap: usize,
    ) -> Result<QueryResult, sqlx::Error> {
        match &self.pool {
            Pool::Postgres(pool) => pg_fetch(pool, sql, binds, cap).await,
            Pool::MySql(pool) => mysql_fetch(pool, sql, binds, cap).await,
            Pool::Sqlite(pool) => sqlite_fetch(pool, sql, binds, cap).await,
        }
    }
}

// ── Schema-row text decoding ────────────────────────────────────────────────
//
// These exist because of a real outage in this connector: MySQL 8's
// `information_schema` hands `TABLE_NAME` back as `VARBINARY` and `DATA_TYPE`
// as `BLOB` (the data dictionary's `utf8mb3_bin` columns), so a plain `String`
// decode fails there. The original code paired that decode with
// `unwrap_or_default()`, which turned every name into `""`, every row was then
// filtered out as empty, and `db_tables` answered "this database has no
// tables" — a silent wrong answer, the worst kind. Text first, bytes second,
// and an undecodable value is an ERROR, never an empty string.

macro_rules! schema_text_fn {
    ($name:ident, $row:ty) => {
        fn $name(row: &$row, column: &str) -> Result<String, sqlx::Error> {
            match row.try_get::<String, _>(column) {
                Ok(s) => Ok(s),
                Err(text_err) => match row.try_get::<Vec<u8>, _>(column) {
                    Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
                    // Report the text error: it names the actual SQL type, which
                    // is what an operator needs to see.
                    Err(_) => Err(text_err),
                },
            }
        }
    };
}

schema_text_fn!(pg_schema_text, PgRow);
schema_text_fn!(mysql_schema_text, MySqlRow);
schema_text_fn!(sqlite_schema_text, SqliteRow);

// ── Per-driver fetch ────────────────────────────────────────────────────────
//
// Each of these is a concrete `async fn` over one pool type, and each binds the
// borrowed connection to an explicitly-typed local before handing it to sqlx.
// Both details are load-bearing: with the reborrow written inline as
// `.execute(&mut *conn)` inside the generic `DbSource::fetch`, rustc reports
// "implementation of `Executor` is not general enough" and the resulting future
// is not `Send` — which the MCP HTTP/SSE transports require (see
// `public_futures_are_send`).

async fn pg_fetch(
    pool: &PgPool,
    sql: &str,
    binds: &[Bind],
    cap: usize,
) -> Result<QueryResult, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let c: &mut sqlx::postgres::PgConnection = &mut conn;
    // `Executor::execute(&str)` rather than `sqlx::raw_sql(…)`: `RawSql` is
    // generic over the `Database`, and that genericity is what makes rustc
    // report "implementation of `Executor` is not general enough" and drop
    // `Send` from this future. A bare `&str` carries no arguments, so sqlx
    // sends it over the simple query protocol — which is also the only way
    // PostgreSQL accepts transaction control.
    c.execute("BEGIN READ ONLY").await?;
    let out = {
        let c: &mut sqlx::postgres::PgConnection = &mut conn;
        let mut q = sqlx::query(sql);
        for b in binds {
            q = bind_pg(q, b);
        }
        collect_rows(q.fetch(c), cap, |r: PgRow| pg_row_to_json(&r)).await
    };
    let c: &mut sqlx::postgres::PgConnection = &mut conn;
    let _ = c.execute("ROLLBACK").await;
    out
}

async fn mysql_fetch(
    pool: &MySqlPool,
    sql: &str,
    binds: &[Bind],
    cap: usize,
) -> Result<QueryResult, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let c: &mut sqlx::mysql::MySqlConnection = &mut conn;
    // See the note in `pg_fetch` for why this is not `sqlx::raw_sql`.
    c.execute("START TRANSACTION READ ONLY").await?;
    let out = {
        let c: &mut sqlx::mysql::MySqlConnection = &mut conn;
        let mut q = sqlx::query(sql);
        for b in binds {
            q = bind_mysql(q, b);
        }
        collect_rows(q.fetch(c), cap, |r: MySqlRow| mysql_row_to_json(&r)).await
    };
    let c: &mut sqlx::mysql::MySqlConnection = &mut conn;
    let _ = c.execute("ROLLBACK").await;
    out
}

/// SQLite needs no transaction: the read-only file handle opened in
/// [`sqlite_options`] means a write cannot reach the file at all.
async fn sqlite_fetch(
    pool: &SqlitePool,
    sql: &str,
    binds: &[Bind],
    cap: usize,
) -> Result<QueryResult, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let c: &mut sqlx::sqlite::SqliteConnection = &mut conn;
    let mut q = sqlx::query(sql);
    for b in binds {
        q = bind_sqlite(q, b);
    }
    collect_rows(q.fetch(c), cap, |r: SqliteRow| sqlite_row_to_json(&r)).await
}

// ── Row collection ──────────────────────────────────────────────────────────

/// Drain at most `cap` rows, setting `truncated` when a `cap + 1`-th row
/// exists. Stops reading the stream at that point, so a `SELECT *` over a
/// million-row table costs `cap + 1` rows, not a million.
async fn collect_rows<R, S, F>(mut stream: S, cap: usize, map: F) -> Result<QueryResult, sqlx::Error>
where
    S: futures_util::Stream<Item = Result<R, sqlx::Error>> + Unpin,
    // Takes the row BY VALUE on purpose. `Fn(&R)` would be a higher-ranked
    // bound (`for<'a> Fn(&'a R)`), and an HRTB here is enough to make the
    // whole future "not general enough" for `Send` — which the MCP HTTP/SSE
    // transports require. See `public_futures_are_send` below.
    F: Fn(R) -> JsonRow,
{
    let mut rows: Vec<JsonRow> = Vec::new();
    let mut truncated = false;
    while let Some(row) = stream.try_next().await? {
        if rows.len() >= cap {
            truncated = true;
            break;
        }
        rows.push(map(row));
    }
    Ok(QueryResult {
        row_count: rows.len(),
        rows,
        truncated,
    })
}

// ── Bind application ────────────────────────────────────────────────────────

type PgQuery<'q> = sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>;
type MySqlQuery<'q> = sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments>;
type SqliteQuery<'q> = sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>;

fn bind_pg<'q>(q: PgQuery<'q>, b: &Bind) -> PgQuery<'q> {
    match b {
        Bind::Str(s) => q.bind(s.clone()),
        Bind::Int(i) => q.bind(*i),
        Bind::Float(f) => q.bind(*f),
        Bind::Bool(v) => q.bind(*v),
    }
}

fn bind_mysql<'q>(q: MySqlQuery<'q>, b: &Bind) -> MySqlQuery<'q> {
    match b {
        Bind::Str(s) => q.bind(s.clone()),
        Bind::Int(i) => q.bind(*i),
        Bind::Float(f) => q.bind(*f),
        Bind::Bool(v) => q.bind(*v),
    }
}

fn bind_sqlite<'q>(q: SqliteQuery<'q>, b: &Bind) -> SqliteQuery<'q> {
    match b {
        Bind::Str(s) => q.bind(s.clone()),
        Bind::Int(i) => q.bind(*i),
        Bind::Float(f) => q.bind(*f),
        Bind::Bool(v) => q.bind(*v),
    }
}

// ── SQL construction (pure, unit-testable without a database) ───────────────

const PG_SCHEMA_SQL: &str = "\
SELECT c.table_name AS table_name, c.column_name AS column_name, c.data_type AS data_type \
FROM information_schema.columns c \
JOIN information_schema.tables t \
  ON t.table_schema = c.table_schema AND t.table_name = c.table_name \
WHERE c.table_schema = current_schema() \
  AND t.table_type IN ('BASE TABLE', 'VIEW') \
ORDER BY c.table_name, c.ordinal_position";

const MYSQL_SCHEMA_SQL: &str = "\
SELECT c.TABLE_NAME AS table_name, c.COLUMN_NAME AS column_name, c.DATA_TYPE AS data_type \
FROM information_schema.COLUMNS c \
JOIN information_schema.TABLES t \
  ON t.TABLE_SCHEMA = c.TABLE_SCHEMA AND t.TABLE_NAME = c.TABLE_NAME \
WHERE c.TABLE_SCHEMA = DATABASE() \
  AND t.TABLE_TYPE IN ('BASE TABLE', 'VIEW') \
ORDER BY c.TABLE_NAME, c.ORDINAL_POSITION";

const SQLITE_TABLES_SQL: &str = "\
SELECT name FROM sqlite_master \
WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite@_%' ESCAPE '@' \
ORDER BY name";

/// Turn a validated [`SelectRequest`] into SQL + binds.
pub(crate) fn build_select(
    cfg: &DbSourceConfig,
    req: &SelectRequest,
    cap: usize,
) -> Result<BuiltSelect, DbError> {
    let driver = cfg.driver;
    let table = req.table.trim();
    if !ident::is_valid_identifier(table) {
        return Err(DbError::InvalidIdentifier(table.to_string()));
    }
    if !cfg.table_allowed(table) {
        return Err(DbError::TableNotAllowed {
            source_name: cfg.name.clone(),
            table: table.to_string(),
        });
    }

    let projection = if req.columns.is_empty() {
        "*".to_string()
    } else {
        let mut parts = Vec::with_capacity(req.columns.len());
        for c in &req.columns {
            let c = c.trim();
            if !ident::is_valid_identifier(c) {
                return Err(DbError::InvalidIdentifier(c.to_string()));
            }
            parts.push(ident::quote_ident(driver, c));
        }
        parts.join(", ")
    };

    if req.filter.len() > MAX_FILTERS {
        return Err(DbError::InvalidFilter(format!(
            "查詢條件過多（{} 條，上限 {MAX_FILTERS} 條）",
            req.filter.len()
        )));
    }

    let mut binds: Vec<Bind> = Vec::new();
    let mut clauses: Vec<String> = Vec::new();
    for f in &req.filter {
        clauses.push(build_filter_clause(driver, f, &mut binds)?);
    }

    let mut sql = format!(
        "SELECT {projection} FROM {}",
        ident::quote_ident(driver, table)
    );
    if !clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&clauses.join(" AND "));
    }
    if let Some(raw) = req.order_by.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let (col, dir) = ident::parse_order_by(raw)
            .ok_or_else(|| DbError::InvalidIdentifier(raw.to_string()))?;
        sql.push_str(&format!(
            " ORDER BY {} {dir}",
            ident::quote_ident(driver, &col)
        ));
    }
    // `cap + 1` so the (cap+1)-th row, if it exists, tells us the answer was
    // truncated. The literal is a clamped `usize` we computed — never operator
    // text — so inlining it carries no injection surface and sidesteps the
    // three drivers' differing opinions about a bound LIMIT's type.
    sql.push_str(&format!(" LIMIT {}", cap.saturating_add(1)));

    Ok(BuiltSelect { sql, binds })
}

fn build_filter_clause(
    driver: Driver,
    f: &Filter,
    binds: &mut Vec<Bind>,
) -> Result<String, DbError> {
    let column = f.column.trim();
    if !ident::is_valid_identifier(column) {
        return Err(DbError::InvalidIdentifier(column.to_string()));
    }
    let quoted = ident::quote_ident(driver, column);

    // NULL has no useful comparison operator in SQL — `col = NULL` is never
    // true. Translate the two meaningful cases and refuse the rest rather than
    // emit a clause that silently matches nothing.
    if f.value.is_null() {
        return match f.op {
            FilterOp::Eq => Ok(format!("{quoted} IS NULL")),
            FilterOp::Ne => Ok(format!("{quoted} IS NOT NULL")),
            _ => Err(DbError::InvalidFilter(format!(
                "欄位「{column}」的 null 值只能搭配 = 或 != 使用"
            ))),
        };
    }

    if f.op == FilterOp::In {
        let arr = f.value.as_array().ok_or_else(|| {
            DbError::InvalidFilter(format!("欄位「{column}」的 in 條件需要一個陣列"))
        })?;
        if arr.is_empty() {
            return Err(DbError::InvalidFilter(format!(
                "欄位「{column}」的 in 條件不可為空陣列"
            )));
        }
        if arr.len() > MAX_IN_VALUES {
            return Err(DbError::InvalidFilter(format!(
                "欄位「{column}」的 in 條件過多（{} 個，上限 {MAX_IN_VALUES} 個）",
                arr.len()
            )));
        }
        let mut placeholders = Vec::with_capacity(arr.len());
        for v in arr {
            binds.push(json_to_bind(column, v)?);
            placeholders.push(ident::placeholder(driver, binds.len()));
        }
        return Ok(format!("{quoted} IN ({})", placeholders.join(", ")));
    }

    if f.op == FilterOp::Like && !f.value.is_string() {
        return Err(DbError::InvalidFilter(format!(
            "欄位「{column}」的 like 條件需要字串值"
        )));
    }
    if f.value.is_array() || f.value.is_object() {
        return Err(DbError::InvalidFilter(format!(
            "欄位「{column}」的條件值必須是字串、數字或布林值"
        )));
    }

    binds.push(json_to_bind(column, &f.value)?);
    Ok(format!(
        "{quoted} {} {}",
        f.op.sql(),
        ident::placeholder(driver, binds.len())
    ))
}

fn json_to_bind(column: &str, v: &serde_json::Value) -> Result<Bind, DbError> {
    match v {
        serde_json::Value::String(s) => Ok(Bind::Str(s.clone())),
        serde_json::Value::Bool(b) => Ok(Bind::Bool(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Bind::Int(i))
            } else if let Some(f) = n.as_f64() {
                Ok(Bind::Float(f))
            } else {
                Err(DbError::InvalidFilter(format!(
                    "欄位「{column}」的數值超出支援範圍"
                )))
            }
        }
        _ => Err(DbError::InvalidFilter(format!(
            "欄位「{column}」的條件值必須是字串、數字或布林值"
        ))),
    }
}

// ── SQLite connect options ──────────────────────────────────────────────────

fn sqlite_options(url: &str) -> Result<SqliteConnectOptions, DbError> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(DbError::Config("SQLite 資料來源缺少檔案路徑".into()));
    }
    let opts = if trimmed.starts_with("sqlite:") {
        SqliteConnectOptions::from_str(trimmed)
            .map_err(|e| DbError::Config(format!("SQLite 連線字串無效：{e}")))?
    } else {
        SqliteConnectOptions::new().filename(trimmed)
    };
    Ok(opts.read_only(true).create_if_missing(false))
}

/// Connect-failure text with the credential stripped.
///
/// For SQLite the "url" is a path, so the file's *base name* is kept: it is
/// not a credential and without it the operator cannot tell which of several
/// sources is broken.
fn connect_err(e: &sqlx::Error, cfg: &DbSourceConfig) -> DbError {
    let base = scrub_connection_details(&e.to_string(), Some(&cfg.url));
    if cfg.driver == Driver::Sqlite {
        let name = std::path::Path::new(cfg.url.trim_start_matches("sqlite://"))
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if !name.is_empty() {
            return DbError::Connect(format!("{base}（檔案：{name}）"));
        }
    }
    DbError::Connect(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DEFAULT_MAX_ROWS, Driver};
    use serde_json::json;

    fn cfg(driver: Driver) -> DbSourceConfig {
        DbSourceConfig::new(
            "demo",
            driver,
            "/tmp/demo.sqlite",
            vec!["customers".into(), "orders".into()],
        )
    }

    fn req(table: &str) -> SelectRequest {
        SelectRequest {
            table: table.into(),
            ..Default::default()
        }
    }

    #[test]
    fn builds_plain_select() {
        let b = build_select(&cfg(Driver::Sqlite), &req("customers"), 10).unwrap();
        assert_eq!(b.sql, "SELECT * FROM \"customers\" LIMIT 11");
        assert!(b.binds.is_empty());
    }

    #[test]
    fn builds_projection_and_filters_per_driver() {
        let mut r = req("customers");
        r.columns = vec!["id".into(), "name".into()];
        r.filter = vec![Filter {
            column: "name".into(),
            op: FilterOp::Eq,
            value: json!("Amy"),
        }];
        r.order_by = Some("id desc".into());

        let pg = build_select(&cfg(Driver::Postgres), &r, 5).unwrap();
        assert_eq!(
            pg.sql,
            "SELECT \"id\", \"name\" FROM \"customers\" WHERE \"name\" = $1 ORDER BY \"id\" DESC LIMIT 6"
        );
        assert_eq!(pg.binds, vec![Bind::Str("Amy".into())]);

        let my = build_select(&cfg(Driver::Mysql), &r, 5).unwrap();
        assert_eq!(
            my.sql,
            "SELECT `id`, `name` FROM `customers` WHERE `name` = ? ORDER BY `id` DESC LIMIT 6"
        );
    }

    #[test]
    fn in_filter_expands_placeholders() {
        let mut r = req("orders");
        r.filter = vec![Filter {
            column: "id".into(),
            op: FilterOp::In,
            value: json!([1, 2, 3]),
        }];
        let b = build_select(&cfg(Driver::Postgres), &r, 2).unwrap();
        assert_eq!(
            b.sql,
            "SELECT * FROM \"orders\" WHERE \"id\" IN ($1, $2, $3) LIMIT 3"
        );
        assert_eq!(
            b.binds,
            vec![Bind::Int(1), Bind::Int(2), Bind::Int(3)]
        );
    }

    #[test]
    fn null_filter_becomes_is_null() {
        let mut r = req("customers");
        r.filter = vec![Filter {
            column: "email".into(),
            op: FilterOp::Eq,
            value: json!(null),
        }];
        let b = build_select(&cfg(Driver::Sqlite), &r, 1).unwrap();
        assert_eq!(
            b.sql,
            "SELECT * FROM \"customers\" WHERE \"email\" IS NULL LIMIT 2"
        );
        assert!(b.binds.is_empty());

        r.filter[0].op = FilterOp::Ne;
        let b = build_select(&cfg(Driver::Sqlite), &r, 1).unwrap();
        assert!(b.sql.contains("IS NOT NULL"));

        r.filter[0].op = FilterOp::Gt;
        assert!(matches!(
            build_select(&cfg(Driver::Sqlite), &r, 1),
            Err(DbError::InvalidFilter(_))
        ));
    }

    #[test]
    fn rejects_bad_identifiers_everywhere() {
        // table
        assert!(matches!(
            build_select(&cfg(Driver::Sqlite), &req("customers; DROP TABLE x"), 1),
            Err(DbError::InvalidIdentifier(_))
        ));
        // column
        let mut r = req("customers");
        r.columns = vec!["name\"; --".into()];
        assert!(matches!(
            build_select(&cfg(Driver::Sqlite), &r, 1),
            Err(DbError::InvalidIdentifier(_))
        ));
        // filter column
        let mut r = req("customers");
        r.filter = vec![Filter {
            column: "a` OR 1=1 --".into(),
            op: FilterOp::Eq,
            value: json!(1),
        }];
        assert!(matches!(
            build_select(&cfg(Driver::Sqlite), &r, 1),
            Err(DbError::InvalidIdentifier(_))
        ));
        // order_by
        let mut r = req("customers");
        r.order_by = Some("id; DELETE FROM customers".into());
        assert!(matches!(
            build_select(&cfg(Driver::Sqlite), &r, 1),
            Err(DbError::InvalidIdentifier(_))
        ));
    }

    #[test]
    fn rejects_table_outside_allowlist() {
        let err = build_select(&cfg(Driver::Sqlite), &req("secrets"), 1).unwrap_err();
        assert!(matches!(err, DbError::TableNotAllowed { .. }));
    }

    #[test]
    fn rejects_too_many_filters_and_in_values() {
        let mut r = req("customers");
        r.filter = (0..MAX_FILTERS + 1)
            .map(|_| Filter {
                column: "id".into(),
                op: FilterOp::Eq,
                value: json!(1),
            })
            .collect();
        assert!(matches!(
            build_select(&cfg(Driver::Sqlite), &r, 1),
            Err(DbError::InvalidFilter(_))
        ));

        let mut r = req("customers");
        let big: Vec<serde_json::Value> =
            (0..MAX_IN_VALUES + 1).map(|i| json!(i)).collect();
        r.filter = vec![Filter {
            column: "id".into(),
            op: FilterOp::In,
            value: serde_json::Value::Array(big),
        }];
        assert!(matches!(
            build_select(&cfg(Driver::Sqlite), &r, 1),
            Err(DbError::InvalidFilter(_))
        ));
    }

    #[test]
    fn like_requires_string_value() {
        let mut r = req("customers");
        r.filter = vec![Filter {
            column: "name".into(),
            op: FilterOp::Like,
            value: json!(5),
        }];
        assert!(matches!(
            build_select(&cfg(Driver::Sqlite), &r, 1),
            Err(DbError::InvalidFilter(_))
        ));
    }

    #[test]
    fn default_max_rows_is_the_ceiling() {
        let c = cfg(Driver::Sqlite);
        assert_eq!(c.max_rows, DEFAULT_MAX_ROWS);
    }

    /// The MCP HTTP/SSE transports hand futures containing these calls to
    /// axum, which requires `Send`. sqlx's `Executor` impls are notoriously
    /// easy to use in a way that is "not general enough" over connection
    /// lifetimes, which silently poisons `Send` three crates away — assert it
    /// here, at the source.
    #[test]
    fn public_futures_are_send() {
        fn assert_send<T: Send>(_: T) {}
        let cfg = cfg(Driver::Sqlite);
        assert_send(DbSource::connect(cfg.clone()));
        let pg: Option<&PgPool> = None;
        if let Some(p) = pg {
            assert_send(pg_fetch(p, "SELECT 1", &[], 1));
        }
        let my: Option<&MySqlPool> = None;
        if let Some(p) = my {
            assert_send(mysql_fetch(p, "SELECT 1", &[], 1));
        }
        let lite: Option<&SqlitePool> = None;
        if let Some(p) = lite {
            assert_send(sqlite_fetch(p, "SELECT 1", &[], 1));
        }
        let dummy: Option<&DbSource> = None;
        if let Some(src) = dummy {
            assert_send(src.ping());
            assert_send(src.list_tables());
            assert_send(src.select(&req("customers")));
            assert_send(src.query("SELECT 1", None));
            assert_send(src.close());
        }
    }

    #[test]
    fn sqlite_options_accept_path_and_scheme() {
        assert!(sqlite_options("/tmp/x.sqlite").is_ok());
        assert!(sqlite_options("sqlite:///tmp/x.sqlite").is_ok());
        assert!(matches!(sqlite_options("   "), Err(DbError::Config(_))));
    }

    // ── Live-database unit tests ────────────────────────────────────────
    // These need `DbSource`'s *private* surface (`run`, `fetch`), which is why
    // they live here rather than in tests/sqlite.rs.

    async fn live_sqlite(dir: &std::path::Path, timeout_ms: u64) -> DbSource {
        let path = dir.join("unit.sqlite");
        let opts = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let pool = SqlitePool::connect_with(opts).await.unwrap();
        sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO t (id, v) VALUES (1, 'a')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        let cfg = DbSourceConfig::new(
            "unit",
            Driver::Sqlite,
            path.to_string_lossy().to_string(),
            vec!["t".into()],
        )
        .with_timeout_ms(timeout_ms);
        DbSource::connect(cfg).await.unwrap()
    }

    /// Layer (c): work that outlives `timeout_ms` comes back as `Timeout`,
    /// not as a hung call.
    #[tokio::test]
    async fn run_enforces_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let src = live_sqlite(dir.path(), 100).await;
        let started = std::time::Instant::now();
        let out = src
            .run(async {
                tokio::time::sleep(Duration::from_millis(5_000)).await;
                42
            })
            .await;
        assert!(matches!(out, Err(DbError::Timeout(100))), "{out:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "should have given up after ~100ms"
        );
        // A call that finishes inside the budget is untouched.
        assert_eq!(src.run(async { 7 }).await.unwrap(), 7);
    }

    /// Layer (b) for SQLite is the read-only file handle. Proven by driving a
    /// write straight through `fetch`, which skips the statement guard — the
    /// database itself has to be the one saying no.
    #[tokio::test]
    async fn sqlite_file_handle_is_read_only_even_without_the_statement_guard() {
        let dir = tempfile::tempdir().unwrap();
        let src = live_sqlite(dir.path(), 5_000).await;
        for sql in [
            "INSERT INTO t (id, v) VALUES (2, 'b')",
            "UPDATE t SET v = 'z'",
            "DELETE FROM t",
            "DROP TABLE t",
        ] {
            let err = src
                .fetch(sql, &[], 10)
                .await
                .expect_err("a read-only handle must refuse {sql}");
            let msg = err.to_string();
            assert!(
                msg.to_ascii_lowercase().contains("readonly")
                    || msg.to_ascii_lowercase().contains("read-only")
                    || msg.to_ascii_lowercase().contains("read only"),
                "{sql}: unexpected error {msg}"
            );
        }
        // The row is still there.
        let out = src.fetch("SELECT COUNT(*) AS n FROM t", &[], 10).await.unwrap();
        assert_eq!(out.rows[0]["n"], serde_json::json!(1));
    }
}
