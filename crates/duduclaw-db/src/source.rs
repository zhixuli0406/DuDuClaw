//! The connected source: pools, schema listing, structured select, free-form
//! read-only query.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures_util::TryStreamExt;
use rusqlite::OpenFlags;
use sqlx::{Executor, Row};
use sqlx::mysql::{MySqlPool, MySqlPoolOptions, MySqlRow};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};

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
    Sqlite(Arc<SqliteEngine>),
}

/// Failure inside one driver call, before it is turned into a [`DbError`].
///
/// The SQLite arm carries pre-rendered text (in sqlx's
/// `error returned from database: (code: N) message` shape, so operators see
/// the same wording as before the move to rusqlite) plus whether the statement
/// was stopped by the deadline.
#[derive(Debug)]
pub(crate) enum EngineError {
    Sqlx(sqlx::Error),
    Sqlite(String),
    /// The SQLite progress handler stopped a statement at `timeout_ms`.
    SqliteInterrupted(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Sqlx(e) => std::fmt::Display::fmt(e, f),
            EngineError::Sqlite(m) | EngineError::SqliteInterrupted(m) => f.write_str(m),
        }
    }
}

impl From<sqlx::Error> for EngineError {
    fn from(e: sqlx::Error) -> Self {
        EngineError::Sqlx(e)
    }
}

impl From<rusqlite::Error> for EngineError {
    fn from(e: rusqlite::Error) -> Self {
        let text = sqlite_error_text(&e);
        if e.sqlite_error_code() == Some(rusqlite::ErrorCode::OperationInterrupted) {
            EngineError::SqliteInterrupted(text)
        } else {
            EngineError::Sqlite(text)
        }
    }
}

/// rusqlite error → the text sqlx used to produce for the same failure.
fn sqlite_error_text(e: &rusqlite::Error) -> String {
    match e.sqlite_error() {
        Some(ffi) => {
            let message = match e {
                rusqlite::Error::SqliteFailure(_, Some(m)) => m.clone(),
                other => other.to_string(),
            };
            format!(
                "error returned from database: (code: {}) {message}",
                ffi.extended_code
            )
        }
        None => e.to_string(),
    }
}

/// A live, read-only handle on one configured data source.
pub type IoAuthority = Arc<dyn Fn() -> bool + Send + Sync>;

fn check_io(authority: &IoAuthority) -> Result<(), sqlx::Error> {
    if authority() { Ok(()) } else { Err(sqlx::Error::Protocol("database authority revoked".into())) }
}

/// Allowlist test shared by [`DbSource::table_allowed`] and the SQLite
/// blocking worker (which cannot borrow the `DbSource`). In exact mode the
/// name must match an allowed entry byte for byte.
fn table_allowed_in(cfg: &DbSourceConfig, exact: bool, table: &str) -> bool {
    if exact { cfg.allowed_tables.iter().any(|name| name.trim() != "*" && name == table) }
    else { cfg.table_allowed(table) }
}

pub struct DbSource {
    cfg: DbSourceConfig,
    pool: Pool,
    authority: IoAuthority,
    exact_table_names: bool,
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
    /// SQLite is opened with a read-only file handle (rusqlite, see
    /// [`SqliteEngine`]). PostgreSQL and MySQL get their read-only guarantee
    /// per statement instead, from the explicit `READ ONLY` transaction
    /// [`DbSource::fetch`] opens.
    ///
    /// A pool-level `after_connect` hook setting the *session* default
    /// transaction characteristics was tried and removed: sqlx 0.8's
    /// `after_connect` closure signature is not general enough over
    /// connection lifetimes, which makes every future that transitively
    /// contains `connect()` non-`Send` — and the MCP HTTP/SSE transports hand
    /// those futures to axum, which requires `Send`. The belt was not worth
    /// losing the transport.
    pub async fn connect(cfg: DbSourceConfig) -> Result<DbSource, DbError> {
        Self::connect_checked(cfg, Arc::new(|| true)).await
    }

    /// Retains live authority through pool waits and every query; callers must
    /// supply a guard which also checks the selected account and product policy.
    pub async fn connect_checked(cfg: DbSourceConfig, authority: IoAuthority) -> Result<DbSource, DbError> {
        check_io(&authority).map_err(|_| DbError::Config("database authority revoked".into()))?;
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
                    .map_err(|e| connect_err(&e.to_string(), &cfg))?;
                Pool::Postgres(p)
            }
            Driver::Mysql => {
                let p = MySqlPoolOptions::new()
                    .max_connections(POOL_MAX_CONNECTIONS)
                    .acquire_timeout(acquire)
                    .connect(&cfg.url)
                    .await
                    .map_err(|e| connect_err(&e.to_string(), &cfg))?;
                Pool::MySql(p)
            }
            Driver::Sqlite => {
                let opts = sqlite_options(&cfg.url)?;
                let engine = Arc::new(SqliteEngine::new(opts, acquire));
                // Like sqlx's `connect_with`, open one connection eagerly so a
                // missing / unreadable file fails here, not on first use.
                engine
                    .warm_up()
                    .await
                    .map_err(|e| connect_err(&e.to_string(), &cfg))?;
                Pool::Sqlite(engine)
            }
        };
        check_io(&authority).map_err(|_| DbError::Config("database authority revoked".into()))?;
        Ok(DbSource { cfg, pool, authority, exact_table_names:false })
    }

    /// Task-limited grants name exact database objects. Preserve case here:
    /// PostgreSQL can contain distinct quoted tables `customers`/`CUSTOMERS`.
    pub fn with_exact_table_names(mut self) -> Self { self.exact_table_names=true;self }

    fn table_allowed(&self,table:&str)->bool { table_allowed_in(&self.cfg,self.exact_table_names,table) }

    pub fn config(&self) -> &DbSourceConfig {
        &self.cfg
    }

    /// Cheapest possible liveness probe — used by `db_sources.test`.
    pub async fn ping(&self) -> Result<(), DbError> {
        // Routed through `fetch` so the probe gets the same live-authority
        // checks (before/after acquire, before the statement) as any query.
        self.run(self.fetch("SELECT 1", &[], 1))
            .await?
            .map(|_| ())
            .map_err(|e| self.engine_err(e))
    }

    /// Tables (and views) in the default schema, filtered by `allowed_tables`.
    pub async fn list_tables(&self) -> Result<Vec<TableInfo>, DbError> {
        let tables = self
            .run(self.list_tables_inner())
            .await?
            .map_err(|e| self.engine_err(e))?;
        Ok(tables)
    }

    async fn list_tables_inner(&self) -> Result<Vec<TableInfo>, EngineError> {
        let (schema_sql, table_names) = schema_query_scoped(&self.cfg,self.exact_table_names);
        check_io(&self.authority)?;
        match &self.pool {
            Pool::Postgres(p) => {
                let mut connection = p.acquire().await?;
                check_io(&self.authority)?;
                let mut query = sqlx::query(&schema_sql);
                for table in &table_names { query = query.bind(table); }
                let rows = query.fetch_all(&mut *connection).await?;
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
                let mut connection = p.acquire().await?;
                check_io(&self.authority)?;
                let mut query = sqlx::query(&schema_sql);
                for table in &table_names { query = query.bind(table); }
                let rows = query.fetch_all(&mut *connection).await?;
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
            Pool::Sqlite(e) => {
                let cfg = self.cfg.clone();
                let exact = self.exact_table_names;
                let authority = Arc::clone(&self.authority);
                e.with_conn(move |conn| {
                    sqlite_list_tables(conn, &cfg, exact, &schema_sql, &table_names, &authority)
                })
                .await
            }
        }
    }

    fn group_schema_rows<I>(&self, rows: I) -> Vec<TableInfo>
    where
        I: Iterator<Item = (String, String, String)>,
    {
        let mut grouped: BTreeMap<String, Vec<ColumnInfo>> = BTreeMap::new();
        for (table, column, data_type) in rows {
            if table.is_empty() || !self.table_allowed(&table) {
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
        if self.exact_table_names && ident::is_valid_identifier(req.table.trim()) && !self.table_allowed(req.table.trim()) { return Err(DbError::TableNotAllowed { source_name:self.cfg.name.clone(),table:req.table.clone() }); }
        let cap = self.effective_limit(req.limit);
        let built = build_select(&self.cfg, req, cap)?;
        self.run(self.fetch(&built.sql, &built.binds, cap))
            .await?
            .map_err(|e| self.engine_err(e))
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
        if self.exact_table_names || !self.cfg.allows_all_tables() {
            return Err(DbError::FreeSqlNotAllowed {
                source_name: self.cfg.name.clone(),
            });
        }
        ensure_read_only_statement(sql)?;
        let cap = self.effective_limit(limit);
        self.run(self.fetch(sql, &[], cap))
            .await?
            .map_err(|e| self.engine_err(e))
    }

    /// Close the underlying pool. Idempotent.
    pub async fn close(&self) {
        match &self.pool {
            Pool::Postgres(p) => p.close().await,
            Pool::MySql(p) => p.close().await,
            Pool::Sqlite(e) => e.close(),
        }
    }

    /// Driver failure → [`DbError`]. A SQLite statement stopped by the
    /// progress-handler deadline is the same event as the `run` deadline
    /// firing, so it gets the same [`DbError::Timeout`] shape whichever of the
    /// two wins the race.
    fn engine_err(&self, e: EngineError) -> DbError {
        match e {
            EngineError::SqliteInterrupted(_) => DbError::Timeout(self.cfg.timeout_ms),
            other => DbError::Query(self.scrub(&other.to_string())),
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
        check_io(&self.authority).map_err(|_| DbError::Config("database authority revoked".into()))?;
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
    ) -> Result<QueryResult, EngineError> {
        match &self.pool {
            Pool::Postgres(pool) => Ok(pg_fetch(pool, sql, binds, cap, &self.authority).await?),
            Pool::MySql(pool) => Ok(mysql_fetch(pool, sql, binds, cap, &self.authority).await?),
            Pool::Sqlite(engine) => engine.fetch(sql, binds, cap, &self.authority).await,
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

/// SQLite twin of the above: text first, bytes second, and a value that is
/// neither is an error rather than an empty string. NULL reads as `""`, which
/// is what sqlx's `String` decode of a NULL SQLite value produced.
fn sqlite_schema_text(
    row: &rusqlite::Row<'_>,
    idx: usize,
    column: &str,
) -> rusqlite::Result<String> {
    use rusqlite::types::ValueRef as V;
    match row.get_ref(idx)? {
        V::Text(b) | V::Blob(b) => Ok(String::from_utf8_lossy(b).into_owned()),
        V::Null => Ok(String::new()),
        other => Err(rusqlite::Error::InvalidColumnType(
            idx,
            column.to_string(),
            other.data_type(),
        )),
    }
}

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
    authority: &IoAuthority,
) -> Result<QueryResult, sqlx::Error> {
    check_io(authority)?;
    let mut conn = pool.acquire().await?;
    check_io(authority)?;
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
        match check_io(authority) {
            Ok(()) => collect_rows(q.fetch(c), cap, |r: PgRow| pg_row_to_json(&r)).await,
            Err(error) => Err(error),
        }
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
    authority: &IoAuthority,
) -> Result<QueryResult, sqlx::Error> {
    check_io(authority)?;
    let mut conn = pool.acquire().await?;
    check_io(authority)?;
    let c: &mut sqlx::mysql::MySqlConnection = &mut conn;
    // See the note in `pg_fetch` for why this is not `sqlx::raw_sql`.
    c.execute("START TRANSACTION READ ONLY").await?;
    let out = {
        let c: &mut sqlx::mysql::MySqlConnection = &mut conn;
        let mut q = sqlx::query(sql);
        for b in binds {
            q = bind_mysql(q, b);
        }
        match check_io(authority) {
            Ok(()) => collect_rows(q.fetch(c), cap, |r: MySqlRow| mysql_row_to_json(&r)).await,
            Err(error) => Err(error),
        }
    };
    let c: &mut sqlx::mysql::MySqlConnection = &mut conn;
    let _ = c.execute("ROLLBACK").await;
    out
}

// ── SQLite engine (rusqlite) ────────────────────────────────────────────────
//
// sqlx's SQLite driver was dropped (RUSTSEC-2026-0459: it pins a
// libsqlite3-sys whose bundled SQLite is vulnerable, and only one crate in the
// graph may link `sqlite3`). This is the smallest replacement that keeps the
// sqlx pool's observable behaviour:
//
// * at most `POOL_MAX_CONNECTIONS` connections in use at once, waited for up
//   to the acquire timeout (`timeout_ms`), idle ones reused;
// * every connection opened `READ_ONLY | NO_MUTEX | URI`, never created, with
//   sqlx's default `busy_timeout` (5 s) and `foreign_keys = ON`;
// * one eager connection at `connect`, so a bad path fails there;
// * after `close`, every call fails and no connection is kept.
//
// rusqlite is blocking, so each call runs on `spawn_blocking`. A runaway
// statement is stopped by a progress handler at the source's deadline — the
// blocking thread cannot be cancelled by dropping the future the way sqlx's
// worker channel could.

/// sqlx's default `SqliteConnectOptions::busy_timeout`.
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// How many SQLite VM instructions run between deadline checks. Small enough
/// that the interrupt lands within a few milliseconds of the deadline.
const SQLITE_PROGRESS_OPS: i32 = 1_000;

/// Parsed SQLite connection target (what `SqliteConnectOptions` held).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SqliteOptions {
    /// Filename handed to `sqlite3_open_v2` — a plain path, `:memory:`, or a
    /// `file:` URI when a URI-only parameter (`immutable`, `vfs`) is set.
    target: String,
    flags: OpenFlags,
}

pub(crate) struct SqliteEngine {
    opts: SqliteOptions,
    /// Acquire timeout *and* per-statement deadline (both are `timeout_ms`).
    timeout: Duration,
    slots: Arc<tokio::sync::Semaphore>,
    idle: Mutex<Vec<rusqlite::Connection>>,
    closed: AtomicBool,
}

impl SqliteEngine {
    fn new(opts: SqliteOptions, timeout: Duration) -> Self {
        SqliteEngine {
            opts,
            timeout,
            slots: Arc::new(tokio::sync::Semaphore::new(POOL_MAX_CONNECTIONS as usize)),
            idle: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
        }
    }

    fn open(&self) -> rusqlite::Result<rusqlite::Connection> {
        let conn = rusqlite::Connection::open_with_flags(&self.opts.target, self.opts.flags)?;
        conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
        conn.execute_batch("PRAGMA foreign_keys = ON")?;
        Ok(conn)
    }

    fn take_idle(&self) -> Option<rusqlite::Connection> {
        self.idle.lock().ok().and_then(|mut v| v.pop())
    }

    fn put_idle(&self, conn: rusqlite::Connection) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        if let Ok(mut v) = self.idle.lock() {
            v.push(conn);
        }
    }

    /// Open the first connection (see `DbSource::connect`).
    async fn warm_up(self: &Arc<Self>) -> Result<(), EngineError> {
        self.with_conn(|_| Ok(())).await
    }

    /// Run `f` on a pooled connection on the blocking pool, with the
    /// statement deadline armed.
    async fn with_conn<T, F>(self: &Arc<Self>, f: F) -> Result<T, EngineError>
    where
        F: FnOnce(&rusqlite::Connection) -> Result<T, EngineError> + Send + 'static,
        T: Send + 'static,
    {
        if self.closed.load(Ordering::SeqCst) {
            return Err(EngineError::Sqlite(
                "attempted to acquire a connection on a closed pool".into(),
            ));
        }
        let permit = match tokio::time::timeout(
            self.timeout,
            Arc::clone(&self.slots).acquire_owned(),
        )
        .await
        {
            Ok(Ok(p)) => p,
            Ok(Err(_)) => {
                return Err(EngineError::Sqlite(
                    "attempted to acquire a connection on a closed pool".into(),
                ));
            }
            Err(_) => {
                return Err(EngineError::Sqlite(
                    "pool timed out while waiting for an open connection".into(),
                ));
            }
        };
        let engine = Arc::clone(self);
        let joined = tokio::task::spawn_blocking(move || -> Result<T, EngineError> {
            // Held for the whole blocking call: a statement that outlived the
            // caller's deadline keeps its slot until it is interrupted.
            let _permit = permit;
            let mut conn = match engine.take_idle() {
                Some(c) => c,
                None => engine.open()?,
            };
            let deadline = Instant::now() + engine.timeout;
            conn.progress_handler(
                SQLITE_PROGRESS_OPS,
                Some(move || Instant::now() >= deadline),
            );
            let out = f(&conn);
            conn.progress_handler(0, None::<fn() -> bool>);
            engine.put_idle(conn);
            out
        })
        .await;
        match joined {
            Ok(r) => r,
            Err(e) => Err(EngineError::Sqlite(format!("SQLite worker failed: {e}"))),
        }
    }

    /// SQLite needs no transaction: the read-only file handle opened in
    /// [`SqliteEngine::open`] means a write cannot reach the file at all.
    ///
    /// Live authority is checked before waiting for a slot, again once the
    /// connection is in hand, and once more right before the statement runs
    /// (the same three points the sqlx path checked).
    async fn fetch(
        self: &Arc<Self>,
        sql: &str,
        binds: &[Bind],
        cap: usize,
        authority: &IoAuthority,
    ) -> Result<QueryResult, EngineError> {
        check_io(authority)?;
        let sql = sql.to_string();
        let binds = binds.to_vec();
        let authority = Arc::clone(authority);
        self.with_conn(move |conn| {
            check_io(&authority)?;
            sqlite_fetch_blocking(conn, &sql, &binds, cap, &authority)
        })
        .await
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.slots.close();
        if let Ok(mut v) = self.idle.lock() {
            v.clear();
        }
    }
}

/// Step at most `cap + 1` rows (see [`collect_rows`] for the contract).
fn sqlite_fetch_blocking(
    conn: &rusqlite::Connection,
    sql: &str,
    binds: &[Bind],
    cap: usize,
    authority: &IoAuthority,
) -> Result<QueryResult, EngineError> {
    let mut stmt = conn.prepare_cached(sql)?;
    let names: Vec<String> = stmt
        .column_names()
        .into_iter()
        .map(str::to_string)
        .collect();
    let params: Vec<rusqlite::types::Value> = binds.iter().map(bind_sqlite).collect();
    check_io(authority)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
    let mut out: Vec<JsonRow> = Vec::new();
    let mut truncated = false;
    while let Some(row) = rows.next()? {
        if out.len() >= cap {
            truncated = true;
            break;
        }
        out.push(sqlite_row_to_json(row, &names));
    }
    Ok(QueryResult {
        row_count: out.len(),
        rows: out,
        truncated,
    })
}

/// SQLite has no information_schema. Names come from sqlite_master (already
/// narrowed at the database by the scoped `schema_sql` / bound `table_names`
/// from [`schema_query_scoped`]); columns from the pragma table-valued
/// function, queried only for tables the allowlist admits.
fn sqlite_list_tables(
    conn: &rusqlite::Connection,
    cfg: &DbSourceConfig,
    exact: bool,
    schema_sql: &str,
    table_names: &[String],
    authority: &IoAuthority,
) -> Result<Vec<TableInfo>, EngineError> {
    check_io(authority)?;
    let mut names = Vec::new();
    {
        let mut stmt = conn.prepare_cached(schema_sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(table_names.iter()))?;
        while let Some(row) = rows.next()? {
            names.push(sqlite_schema_text(row, 0, "name")?);
        }
    }
    let mut out = Vec::new();
    let mut cols_stmt = conn.prepare_cached("SELECT name, type FROM pragma_table_info(?)")?;
    for name in names {
        if name.is_empty() || !table_allowed_in(cfg, exact, &name) {
            continue;
        }
        if out.len() >= MAX_TABLES {
            break;
        }
        check_io(authority)?;
        let mut columns = Vec::new();
        let mut rows = cols_stmt.query([name.clone()])?;
        while let Some(c) = rows.next()? {
            columns.push(ColumnInfo {
                name: sqlite_schema_text(c, 0, "name")?,
                data_type: sqlite_schema_text(c, 1, "type")?,
            });
        }
        out.push(TableInfo { name, columns });
    }
    Ok(out)
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

/// sqlx bound a SQLite `bool` as the integer 0 / 1; so does this.
fn bind_sqlite(b: &Bind) -> rusqlite::types::Value {
    use rusqlite::types::Value as V;
    match b {
        Bind::Str(s) => V::Text(s.clone()),
        Bind::Int(i) => V::Integer(*i),
        Bind::Float(f) => V::Real(*f),
        Bind::Bool(v) => V::Integer(i64::from(*v)),
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

// Restrict metadata at the database boundary as well as in the returned rows.
// Allowed identifiers are bound values, never SQL fragments.
#[cfg(test)]
fn schema_query(cfg: &DbSourceConfig) -> (String, Vec<String>) { schema_query_scoped(cfg,false) }
fn schema_query_scoped(cfg: &DbSourceConfig,exact:bool) -> (String, Vec<String>) {
    let (base, column) = match cfg.driver {
        Driver::Postgres => (PG_SCHEMA_SQL, "c.table_name"),
        Driver::Mysql => (MYSQL_SCHEMA_SQL, "c.TABLE_NAME"),
        Driver::Sqlite => (SQLITE_TABLES_SQL, "name"),
    };
    if cfg.allows_all_tables() && !exact { return (base.to_owned(), Vec::new()); }
    let names: Vec<String> = cfg.allowed_tables.iter()
        .filter(|name| !(exact && name.trim() == "*"))
        .map(|name| if exact {name.clone()} else {name.trim().to_ascii_lowercase()}).collect();
    let (prefix, order) = base.split_once("ORDER BY").expect("constant schema query has ordering");
    let predicate = if names.is_empty() {
        "1 = 0".to_owned()
    } else {
        let placeholders: Vec<String> = (1..=names.len()).map(|index| match cfg.driver {
            Driver::Postgres => format!("${index}"),
            _ => "?".to_owned(),
        }).collect();
        if exact {
            let exact_column=match cfg.driver {
                Driver::Postgres=>format!("CAST({column} AS TEXT) COLLATE \"C\""),
                Driver::Mysql=>format!("BINARY {column}"),
                Driver::Sqlite=>format!("{column} COLLATE BINARY"),
            };
            format!("{exact_column} IN ({})",placeholders.join(", "))
        } else {format!("LOWER({column}) IN ({})", placeholders.join(", "))}
    };
    (format!("{prefix} AND {predicate} ORDER BY{order}"), names)
}

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

/// Parse a SQLite source `url` the way sqlx's `SqliteConnectOptions` did,
/// then force read-only / never-create.
///
/// * A bare value is a filesystem path, used verbatim.
/// * `sqlite:` / `sqlite://` URLs: the path part is percent-decoded,
///   `:memory:` is an in-memory database, and the query parameters `mode`
///   (`ro` / `rw` / `rwc` / `memory`), `cache` (`private` / `shared`),
///   `immutable` (`true` / `1` / `false` / `0`) and `vfs` are honoured;
///   anything else is refused. `mode=rw` / `rwc` parse but cannot lift the
///   read-only flag.
/// * `immutable` / `vfs` are URI-only, so they turn the target into a
///   `file:<encoded path>?…` URI (the connection is always opened with
///   `SQLITE_OPEN_URI`, as sqlx did).
fn sqlite_options(url: &str) -> Result<SqliteOptions, DbError> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(DbError::Config("SQLite 資料來源缺少檔案路徑".into()));
    }
    let base_flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_URI;
    if !trimmed.starts_with("sqlite:") {
        return Ok(SqliteOptions {
            target: trimmed.to_string(),
            flags: base_flags | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE,
        });
    }

    let invalid =
        |m: String| DbError::Config(format!("SQLite 連線字串無效：error with configuration: {m}"));

    let rest = trimmed
        .trim_start_matches("sqlite://")
        .trim_start_matches("sqlite:");
    let mut parts = rest.splitn(2, '?');
    let database = parts.next().unwrap_or_default();
    let params = parts.next();

    let mut in_memory = false;
    let mut shared_cache = false;
    let mut immutable = false;
    let mut vfs: Option<String> = None;
    let mut path = String::new();
    if database == ":memory:" {
        in_memory = true;
        shared_cache = true;
    } else {
        path = percent_decode(database, false).map_err(invalid)?;
    }

    if let Some(params) = params {
        for pair in params.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let key = percent_decode(k, true).map_err(invalid)?;
            let value = percent_decode(v, true).map_err(invalid)?;
            match key.as_str() {
                "mode" => match value.as_str() {
                    "ro" | "rw" | "rwc" => {}
                    "memory" => {
                        in_memory = true;
                        shared_cache = true;
                    }
                    _ => return Err(invalid(format!("unknown value {value:?} for `mode`"))),
                },
                "cache" => match value.as_str() {
                    "private" => shared_cache = false,
                    "shared" => shared_cache = true,
                    _ => return Err(invalid(format!("unknown value {value:?} for `cache`"))),
                },
                "immutable" => match value.as_str() {
                    "true" | "1" => immutable = true,
                    "false" | "0" => immutable = false,
                    _ => {
                        return Err(invalid(format!(
                            "unknown value {value:?} for `immutable`"
                        )));
                    }
                },
                "vfs" => vfs = Some(value),
                _ => {
                    return Err(invalid(format!(
                        "unknown query parameter `{key}` while parsing connection URL"
                    )));
                }
            }
        }
    }

    let mut flags = base_flags;
    flags |= if shared_cache {
        OpenFlags::SQLITE_OPEN_SHARED_CACHE
    } else {
        OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
    };

    let target = if in_memory {
        // sqlx named each in-memory database uniquely; same here, so two
        // sources never share one by accident.
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        flags |= OpenFlags::SQLITE_OPEN_MEMORY;
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        format!("file:duduclaw-db-in-memory-{n}")
    } else {
        path
    };

    let mut query: Vec<String> = Vec::new();
    if immutable {
        query.push("immutable=true".into());
    }
    if let Some(v) = vfs {
        query.push(format!("vfs={}", percent_encode(&v, false)));
    }
    let target = if query.is_empty() {
        target
    } else if in_memory {
        format!("{target}?{}", query.join("&"))
    } else {
        format!("file:{}?{}", percent_encode(&target, true), query.join("&"))
    };
    Ok(SqliteOptions { target, flags })
}

/// `%HH` decoding (and `+` → space for query components, as
/// `application/x-www-form-urlencoded` does). Byte-wise, then UTF-8 checked.
fn percent_decode(s: &str, plus_is_space: bool) -> Result<String, String> {
    fn hex(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push(h * 16 + l);
                        i += 3;
                    }
                    _ => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' if plus_is_space => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|e| e.to_string())
}

/// Percent-encode everything but RFC 3986 unreserved characters (and `/` when
/// `keep_slash`), for building a SQLite `file:` URI.
fn percent_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'.' | b'_' | b'~')
            || (keep_slash && b == b'/');
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Connect-failure text with the credential stripped.
///
/// For SQLite the "url" is a path, so the file's *base name* is kept: it is
/// not a credential and without it the operator cannot tell which of several
/// sources is broken.
fn connect_err(message: &str, cfg: &DbSourceConfig) -> DbError {
    let base = scrub_connection_details(message, Some(&cfg.url));
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
    fn metadata_query_only_fetches_granted_tables() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE customers(id INTEGER);\n\
             CREATE TABLE unrelated_secret(id INTEGER);",
        )
        .unwrap();
        let names_for = |sql: &str, binds: &[String]| -> Vec<String> {
            let mut stmt = conn.prepare(sql).unwrap();
            stmt.query_map(rusqlite::params_from_iter(binds.iter()), |r| {
                r.get::<_, String>("name")
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap()
        };
        let mut config = cfg(Driver::Sqlite);
        config.allowed_tables = vec![" CUSTOMERS ".into()];
        let (sql, names) = schema_query(&config);
        assert_eq!(names_for(&sql, &names), ["customers"]);
        config.allowed_tables.clear();
        let (sql, names) = schema_query(&config);
        assert!(names.is_empty());
        assert!(names_for(&sql, &names).is_empty());
        for driver in [Driver::Postgres, Driver::Mysql] {
            let (sql, names) = schema_query(&cfg(driver));
            assert_eq!(names, ["customers", "orders"]);
            assert!(!sql.contains("customers"));
            assert!(sql.contains(if driver == Driver::Postgres { "IN ($1, $2)" } else { "IN (?, ?)" }));
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
            assert_send(pg_fetch(p, "SELECT 1", &[], 1, &(Arc::new(|| true) as IoAuthority)));
        }
        let my: Option<&MySqlPool> = None;
        if let Some(p) = my {
            assert_send(mysql_fetch(p, "SELECT 1", &[], 1, &(Arc::new(|| true) as IoAuthority)));
        }
        let lite: Option<&Arc<SqliteEngine>> = None;
        if let Some(e) = lite {
            assert_send(e.fetch("SELECT 1", &[], 1, &(Arc::new(|| true) as IoAuthority)));
            assert_send(e.warm_up());
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

    #[test]
    fn sqlite_options_parse_like_sqlx_and_stay_read_only() {
        let ro = OpenFlags::SQLITE_OPEN_READ_ONLY;
        let rw = OpenFlags::SQLITE_OPEN_READ_WRITE;
        let create = OpenFlags::SQLITE_OPEN_CREATE;

        let o = sqlite_options("/tmp/x.sqlite").unwrap();
        assert_eq!(o.target, "/tmp/x.sqlite");
        assert!(o.flags.contains(ro | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_URI));
        assert!(!o.flags.intersects(rw | create));

        let o = sqlite_options("sqlite:///tmp/x.sqlite").unwrap();
        assert_eq!(o.target, "/tmp/x.sqlite");
        let o = sqlite_options("sqlite://rel%20dir/x.sqlite").unwrap();
        assert_eq!(o.target, "rel dir/x.sqlite");

        // `mode=rwc` parses but cannot lift read-only.
        let o = sqlite_options("sqlite:///tmp/x.sqlite?mode=rwc").unwrap();
        assert!(o.flags.contains(ro));
        assert!(!o.flags.intersects(rw | create));

        let o = sqlite_options("sqlite:///tmp/x.sqlite?immutable=1").unwrap();
        assert_eq!(o.target, "file:/tmp/x.sqlite?immutable=true");

        let o = sqlite_options("sqlite:///tmp/x.sqlite?cache=shared").unwrap();
        assert!(o.flags.contains(OpenFlags::SQLITE_OPEN_SHARED_CACHE));

        for bad in [
            "sqlite:///tmp/x.sqlite?mode=bogus",
            "sqlite:///tmp/x.sqlite?immutable=maybe",
            "sqlite:///tmp/x.sqlite?journal_mode=wal",
        ] {
            assert!(matches!(sqlite_options(bad), Err(DbError::Config(_))), "{bad}");
        }
    }

    // ── Live-database unit tests ────────────────────────────────────────
    // These need `DbSource`'s *private* surface (`run`, `fetch`), which is why
    // they live here rather than in tests/sqlite.rs.

    /// Create `<dir>/<file>` read-write, run `setup`, close it.
    fn make_db(dir: &std::path::Path, file: &str, setup: &str) -> String {
        let path = dir.join(file);
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(setup).unwrap();
        drop(conn);
        path.to_string_lossy().to_string()
    }

    async fn live_sqlite(dir: &std::path::Path, timeout_ms: u64) -> DbSource {
        let path = make_db(
            dir,
            "unit.sqlite",
            "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT);\n\
             INSERT INTO t (id, v) VALUES (1, 'a');",
        );
        let cfg = DbSourceConfig::new("unit", Driver::Sqlite, path, vec!["t".into()])
            .with_timeout_ms(timeout_ms);
        DbSource::connect(cfg).await.unwrap()
    }

    /// The rusqlite engine itself refuses writes — through `fetch` (no
    /// statement guard), with the same `readonly` wording sqlx surfaced, and
    /// as a `DbError::Query` on the public path.
    #[tokio::test]
    async fn rusqlite_engine_refuses_writes() {
        let dir = tempfile::tempdir().unwrap();
        let src = live_sqlite(dir.path(), 5_000).await;
        let err = src
            .fetch("INSERT INTO t (id, v) VALUES (9, 'x')", &[], 10)
            .await
            .expect_err("read-only handle must refuse the insert");
        let msg = err.to_string();
        assert!(
            msg.starts_with("error returned from database: (code: 8)"),
            "sqlx-shaped error text expected, got {msg}"
        );
        assert!(msg.contains("readonly"), "{msg}");
        // Bound parameters do not change that.
        let err = src
            .fetch(
                "UPDATE t SET v = ? WHERE id = ?",
                &[Bind::Str("z".into()), Bind::Int(1)],
                10,
            )
            .await
            .expect_err("read-only handle must refuse the update");
        assert!(err.to_string().contains("readonly"), "{err}");
        // A schema change through a pragma is refused too.
        assert!(src.fetch("PRAGMA user_version = 7", &[], 10).await.is_err());

        // On the public path the statement guard still fires first, so a
        // write via free SQL keeps reporting StatementRejected.
        let cfg = DbSourceConfig::new("w", Driver::Sqlite, src.cfg.url.clone(), vec!["*".into()]);
        let wild = DbSource::connect(cfg).await.unwrap();
        assert!(matches!(
            wild.query("DELETE FROM t", None).await,
            Err(DbError::StatementRejected(_))
        ));

        let out = src.fetch("SELECT v FROM t WHERE id = 1", &[], 10).await.unwrap();
        assert_eq!(out.rows[0]["v"], serde_json::json!("a"));
    }

    /// Storage-class-driven mapping, value by value, exactly as the sqlx path
    /// produced it.
    #[tokio::test]
    async fn rusqlite_engine_maps_values_like_before() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_db(
            dir.path(),
            "values.sqlite",
            "CREATE TABLE v (\
                 i INTEGER, r REAL, t TEXT, b BLOB, n TEXT, flag BOOLEAN, \
                 ts DATETIME, money NUMERIC, big INTEGER, bad TEXT, untyped);\n\
             INSERT INTO v VALUES (\
                 42, 12.5, '王小明', x'0102', NULL, 1, \
                 '2026-11-09 10:20:30', '12.50', 9223372036854775807, \
                 CAST(x'ff' AS TEXT), 'free');",
        );
        let cfg = DbSourceConfig::new("vals", Driver::Sqlite, path, vec!["*".into()]);
        let src = DbSource::connect(cfg).await.unwrap();
        let out = src.query("SELECT * FROM v", None).await.unwrap();
        assert_eq!(out.row_count, 1);
        let row = &out.rows[0];
        let keys: Vec<&str> = row.keys().map(String::as_str).collect();
        assert_eq!(keys.len(), 11, "{keys:?}");
        assert_eq!(row["i"], serde_json::json!(42));
        assert_eq!(row["r"], serde_json::json!(12.5));
        assert_eq!(row["t"], serde_json::json!("王小明"));
        assert_eq!(row["b"], serde_json::json!("AQI="));
        assert_eq!(row["n"], serde_json::Value::Null);
        // BOOLEAN is a hint; the stored INTEGER is the truth.
        assert_eq!(row["flag"], serde_json::json!(1));
        // DATETIME holds TEXT: passed through as stored.
        assert_eq!(row["ts"], serde_json::json!("2026-11-09 10:20:30"));
        // NUMERIC affinity turned '12.50' into REAL 12.5.
        assert_eq!(row["money"], serde_json::json!(12.5));
        assert_eq!(row["big"], serde_json::json!(i64::MAX));
        // Non-UTF-8 TEXT decodes to null, as sqlx's String decode failure did.
        assert_eq!(row["bad"], serde_json::Value::Null);
        assert_eq!(row["untyped"], serde_json::json!("free"));

        // Computed values: integer division, float, NULL, and bound params.
        let out = src
            .fetch(
                "SELECT 7 / 2 AS q, 1.0 / 4 AS f, NULL AS z, ? AS s, ? AS yes, ? AS x",
                &[Bind::Str("é".into()), Bind::Bool(true), Bind::Float(0.5)],
                10,
            )
            .await
            .unwrap();
        let row = &out.rows[0];
        assert_eq!(row["q"], serde_json::json!(3));
        assert_eq!(row["f"], serde_json::json!(0.25));
        assert_eq!(row["z"], serde_json::Value::Null);
        assert_eq!(row["s"], serde_json::json!("é"));
        assert_eq!(row["yes"], serde_json::json!(1));
        assert_eq!(row["x"], serde_json::json!(0.5));
    }

    /// `cap + 1` rows are stepped, never more, and `truncated` says so.
    #[tokio::test]
    async fn rusqlite_engine_honours_row_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_db(
            dir.path(),
            "cap.sqlite",
            "CREATE TABLE n (x INTEGER);\n\
             WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 50) \
             INSERT INTO n SELECT x FROM c;",
        );
        let cfg = DbSourceConfig::new("cap", Driver::Sqlite, path, vec!["*".into()])
            .with_max_rows(5);
        let src = DbSource::connect(cfg).await.unwrap();

        let out = src.fetch("SELECT x FROM n ORDER BY x", &[], 3).await.unwrap();
        assert_eq!(out.row_count, 3);
        assert!(out.truncated);
        assert_eq!(out.rows[2]["x"], serde_json::json!(3));

        // Exactly `cap` rows is not truncation.
        let out = src.fetch("SELECT x FROM n WHERE x <= 3", &[], 3).await.unwrap();
        assert_eq!(out.row_count, 3);
        assert!(!out.truncated);

        // Public paths apply max_rows as the ceiling.
        let out = src.query("SELECT x FROM n", Some(100)).await.unwrap();
        assert_eq!(out.row_count, 5);
        assert!(out.truncated);
        let out = src
            .select(&SelectRequest {
                table: "n".into(),
                limit: Some(2),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(out.row_count, 2);
        assert!(out.truncated);

        // An unbounded generator stops at cap + 1 rather than running forever.
        let out = src
            .fetch(
                "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT x FROM c",
                &[],
                4,
            )
            .await
            .unwrap();
        assert_eq!(out.row_count, 4);
        assert!(out.truncated);
    }

    /// A statement that never finishes is interrupted at `timeout_ms`, answers
    /// `Timeout`, and frees its connection for the next call.
    #[tokio::test]
    async fn rusqlite_engine_interrupts_runaway_statements() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_db(dir.path(), "slow.sqlite", "CREATE TABLE t (x INTEGER);");
        let cfg = DbSourceConfig::new("slow", Driver::Sqlite, path, vec!["*".into()])
            .with_timeout_ms(100);
        let src = DbSource::connect(cfg).await.unwrap();
        let started = std::time::Instant::now();
        let err = src
            .query(
                "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) \
                 SELECT count(*) FROM c",
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DbError::Timeout(100)), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
        // Both slots are usable again once the interrupted statement unwinds.
        for _ in 0..3 {
            src.ping().await.expect("engine recovers after an interrupt");
        }
    }

    #[tokio::test]
    async fn rusqlite_engine_close_refuses_further_work() {
        let dir = tempfile::tempdir().unwrap();
        let src = live_sqlite(dir.path(), 5_000).await;
        src.close().await;
        src.close().await; // idempotent
        assert!(matches!(src.ping().await, Err(DbError::Query(_))));
    }

    #[tokio::test]
    async fn live_authority_revokes_queries_and_schema_after_pool_creation() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let source = live_sqlite(dir.path(), 5_000).await;
        let config = source.config().clone();
        source.close().await;
        let live = Arc::new(AtomicBool::new(true));
        let flag = live.clone();
        let source = DbSource::connect_checked(config.clone(), Arc::new(move || flag.load(Ordering::SeqCst)))
            .await.unwrap();
        assert!(source.select(&req("t")).await.is_ok());
        live.store(false, Ordering::SeqCst);
        assert!(source.select(&req("t")).await.is_err());
        assert!(source.list_tables().await.is_err());
        assert!(source.ping().await.is_err());
        source.close().await;
        assert!(DbSource::connect_checked(config, Arc::new(|| false)).await.is_err());
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
    #[tokio::test]
    async fn exact_table_mode_preserves_signed_object_case_for_select_and_metadata() {
        let home=tempfile::tempdir().unwrap();
        let mut source=live_sqlite(home.path(),1000).await;
        source.cfg.allowed_tables=vec!["t".into()];
        let source=source.with_exact_table_names();
        assert!(source.select(&req("T")).await.is_err());
        assert_eq!(source.select(&req("t")).await.unwrap().row_count,1);
        let tables=source.group_schema_rows(vec![("t".into(),"id".into(),"INTEGER".into()),
            ("T".into(),"SECRET".into(),"TEXT".into())].into_iter());
        assert_eq!(tables.len(),1);assert_eq!(tables[0].name,"t");
        for driver in [Driver::Postgres,Driver::Mysql,Driver::Sqlite] {
            let config=cfg(driver);let(sql,names)=schema_query_scoped(&config,true);
            assert!(!sql.contains("LOWER("));assert_eq!(names,["customers","orders"]);
            assert!(sql.contains(match driver {Driver::Postgres=>"COLLATE \"C\"",Driver::Mysql=>"BINARY",Driver::Sqlite=>"COLLATE BINARY"}));
        }
        source.close().await;
    }

    #[tokio::test]
    async fn exact_mode_refuses_free_sql_even_with_wildcard() {
        let home=tempfile::tempdir().unwrap();
        let mut source=live_sqlite(home.path(),1000).await;
        source.cfg.allowed_tables=vec!["*".into()];
        let source=source.with_exact_table_names();
        assert!(matches!(source.query("SELECT 1",None).await,Err(DbError::FreeSqlNotAllowed{..})));
        source.close().await;
    }

    #[tokio::test]
    async fn exact_mode_wildcard_grants_no_table() {
        let home=tempfile::tempdir().unwrap();
        let mut source=live_sqlite(home.path(),1000).await;
        source.cfg.allowed_tables=vec![" * ".into()];
        let source=source.with_exact_table_names();
        assert!(!source.table_allowed("t"));
        assert!(!source.table_allowed("*"));
        assert!(source.select(&req("t")).await.is_err());
        let (_sql,names)=schema_query_scoped(&source.cfg,true);
        assert!(names.is_empty());
        source.close().await;
    }

}
