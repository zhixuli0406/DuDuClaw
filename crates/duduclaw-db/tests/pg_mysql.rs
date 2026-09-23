//! PostgreSQL / MySQL behaviour against a real server.
//!
//! These need a live database, so they are `#[ignore]`d by default and also
//! self-skip when the URL env var is absent — running
//! `cargo test -p duduclaw-db -- --ignored` on a machine without Docker is a
//! pass, not a failure.
//!
//! ```bash
//! docker run --rm -e POSTGRES_PASSWORD=pw -p 5432:5432 postgres:16
//! export DUDUCLAW_TEST_PG_URL='postgres://postgres:pw@127.0.0.1:5432/postgres'
//!
//! docker run --rm -e MYSQL_ROOT_PASSWORD=pw -e MYSQL_DATABASE=demo -p 3306:3306 mysql:8
//! export DUDUCLAW_TEST_MYSQL_URL='mysql://root:pw@127.0.0.1:3306/demo'
//!
//! cargo test -p duduclaw-db -- --ignored
//! ```
//!
//! Each test uses a uniquely-named table and drops it afterwards through its
//! own writable connection, so a shared server stays usable.

use duduclaw_db::{DbError, DbSource, DbSourceConfig, Driver, Filter, FilterOp, SelectRequest};
use serde_json::json;
use sqlx::{Executor, mysql::MySqlPool, postgres::PgPool};

fn unique(prefix: &str) -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}_{n:x}")
}

// ── PostgreSQL ──────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "needs DUDUCLAW_TEST_PG_URL and a live PostgreSQL"]
async fn postgres_end_to_end() {
    let Ok(url) = std::env::var("DUDUCLAW_TEST_PG_URL") else {
        eprintln!("DUDUCLAW_TEST_PG_URL not set — skipping");
        return;
    };
    let table = unique("ddc_customers");
    let admin = PgPool::connect(&url).await.expect("admin connect");
    admin
        .execute(
            format!(
                "CREATE TABLE {table} (\
                    id BIGINT PRIMARY KEY, \
                    name TEXT, \
                    balance NUMERIC(10,2), \
                    active BOOLEAN, \
                    created_at TIMESTAMPTZ, \
                    payload JSONB, \
                    blob BYTEA)"
            )
            .as_str(),
        )
        .await
        .unwrap();
    admin
        .execute(
            format!(
                "INSERT INTO {table} VALUES \
                 (1, '王小明', 12.50, true, '2026-09-22T03:04:05Z', '{{\"k\":1}}', '\\x0102'), \
                 (2, 'Amy', 0.00, false, NULL, NULL, NULL), \
                 (3, 'Bob', -7.25, true, '2026-01-02T00:00:00Z', '[1,2]', NULL)"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let cfg = DbSourceConfig::new("pg", Driver::Postgres, url.clone(), vec![table.clone()]);
    let src = DbSource::connect(cfg).await.expect("source connect");

    // Schema listing respects the allowlist.
    let tables = src.list_tables().await.unwrap();
    assert_eq!(tables.len(), 1, "{tables:?}");
    assert_eq!(tables[0].name, table);

    // Value mapping: NUMERIC → string, TIMESTAMPTZ → RFC3339, JSONB → parsed,
    // BYTEA → base64, NULL → null.
    let out = src
        .select(&SelectRequest {
            table: table.clone(),
            order_by: Some("id".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(out.row_count, 3);
    assert_eq!(out.rows[0]["id"], json!(1));
    assert_eq!(out.rows[0]["balance"], json!("12.50"));
    assert_eq!(out.rows[0]["active"], json!(true));
    assert!(
        out.rows[0]["created_at"]
            .as_str()
            .unwrap()
            .starts_with("2026-09-22T03:04:05"),
        "{:?}",
        out.rows[0]["created_at"]
    );
    assert_eq!(out.rows[0]["payload"], json!({"k": 1}));
    assert_eq!(out.rows[0]["blob"], json!("AQI="));
    assert_eq!(out.rows[1]["created_at"], serde_json::Value::Null);

    // Filters bind values.
    let out = src
        .select(&SelectRequest {
            table: table.clone(),
            filter: vec![Filter {
                column: "name".into(),
                op: FilterOp::Eq,
                value: json!("x' OR '1'='1"),
            }],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(out.row_count, 0);

    // Truncation.
    let cfg = DbSourceConfig::new("pg", Driver::Postgres, url.clone(), vec![table.clone()])
        .with_max_rows(2);
    let small = DbSource::connect(cfg).await.unwrap();
    let out = small
        .select(&SelectRequest {
            table: table.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(out.row_count, 2);
    assert!(out.truncated);

    // Free-form SQL is refused outright on a source that has a real table
    // allowlist — the allowlist cannot be enforced against arbitrary SQL.
    let err = src
        .query(&format!("SELECT count(*) FROM {table}"), None)
        .await
        .unwrap_err();
    assert!(matches!(err, DbError::FreeSqlNotAllowed { .. }), "{err:?}");

    // Everything below needs a wildcard source, which is how an operator opts
    // into free SQL.
    let free_cfg =
        DbSourceConfig::new("pg_free", Driver::Postgres, url.clone(), vec!["*".into()]);
    let free = DbSource::connect(free_cfg).await.expect("free connect");

    // Statement guard.
    let err = free
        .query(&format!("DELETE FROM {table}"), None)
        .await
        .unwrap_err();
    assert!(matches!(err, DbError::StatementRejected(_)), "{err:?}");

    // Layer (b): a data-modifying CTE passes the statement guard and must be
    // stopped by the READ ONLY transaction.
    let err = free
        .query(
            &format!("WITH x AS (DELETE FROM {table} RETURNING *) SELECT * FROM x"),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DbError::Query(_)), "{err:?}");

    // Nothing was deleted.
    let out = free
        .query(&format!("SELECT count(*) AS n FROM {table}"), None)
        .await
        .unwrap();
    assert_eq!(out.rows[0]["n"], json!(3));

    // A simple-query (text-format) NUMERIC renders identically to the
    // binary-format one the prepared `db_select` path produces.
    let out = free
        .query(&format!("SELECT balance FROM {table} ORDER BY id LIMIT 1"), None)
        .await
        .unwrap();
    assert_eq!(out.rows[0]["balance"], json!("12.50"));

    free.close().await;
    src.close().await;
    admin
        .execute(format!("DROP TABLE {table}").as_str())
        .await
        .unwrap();
    admin.close().await;
}

// ── MySQL ───────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "needs DUDUCLAW_TEST_MYSQL_URL and a live MySQL"]
async fn mysql_end_to_end() {
    let Ok(url) = std::env::var("DUDUCLAW_TEST_MYSQL_URL") else {
        eprintln!("DUDUCLAW_TEST_MYSQL_URL not set — skipping");
        return;
    };
    let table = unique("ddc_customers");
    let admin = MySqlPool::connect(&url).await.expect("admin connect");
    admin
        .execute(
            format!(
                "CREATE TABLE {table} (\
                    id BIGINT PRIMARY KEY, \
                    name VARCHAR(64), \
                    balance DECIMAL(10,2), \
                    active TINYINT(1), \
                    created_at DATETIME, \
                    payload JSON, \
                    blob_col BLOB)"
            )
            .as_str(),
        )
        .await
        .unwrap();
    admin
        .execute(
            format!(
                "INSERT INTO {table} VALUES \
                 (1, '王小明', 12.50, 1, '2026-09-22 03:04:05', '{{\"k\":1}}', 0x0102), \
                 (2, 'Amy', 0.00, 0, NULL, NULL, NULL), \
                 (3, 'Bob', -7.25, 1, '2026-01-02 00:00:00', NULL, NULL)"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let cfg = DbSourceConfig::new("my", Driver::Mysql, url.clone(), vec![table.clone()]);
    let src = DbSource::connect(cfg).await.expect("source connect");

    let tables = src.list_tables().await.unwrap();
    assert_eq!(tables.len(), 1, "{tables:?}");
    assert_eq!(tables[0].name, table);

    let out = src
        .select(&SelectRequest {
            table: table.clone(),
            order_by: Some("id".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(out.row_count, 3);
    assert_eq!(out.rows[0]["balance"], json!("12.50"));
    assert!(
        out.rows[0]["created_at"]
            .as_str()
            .unwrap()
            .starts_with("2026-09-22T03:04:05"),
        "{:?}",
        out.rows[0]["created_at"]
    );
    assert_eq!(out.rows[0]["payload"], json!({"k": 1}));
    assert_eq!(out.rows[0]["blob_col"], json!("AQI="));
    assert_eq!(out.rows[1]["created_at"], serde_json::Value::Null);

    let cfg = DbSourceConfig::new("my", Driver::Mysql, url.clone(), vec![table.clone()])
        .with_max_rows(2);
    let small = DbSource::connect(cfg).await.unwrap();
    let out = small
        .select(&SelectRequest {
            table: table.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(out.row_count, 2);
    assert!(out.truncated);

    // Free SQL is refused on an allowlisted source.
    let err = src
        .query(&format!("SELECT count(*) FROM {table}"), None)
        .await
        .unwrap_err();
    assert!(matches!(err, DbError::FreeSqlNotAllowed { .. }), "{err:?}");

    let free_cfg = DbSourceConfig::new("my_free", Driver::Mysql, url.clone(), vec!["*".into()]);
    let free = DbSource::connect(free_cfg).await.expect("free connect");

    let err = free
        .query(&format!("UPDATE {table} SET name = 'x'"), None)
        .await
        .unwrap_err();
    assert!(matches!(err, DbError::StatementRejected(_)), "{err:?}");

    // Layer (b): MySQL's READ ONLY transaction must refuse a write that the
    // statement guard lets through (a `SELECT ... FOR UPDATE` acquires write
    // locks, which a read-only transaction cannot do).
    let err = free
        .query(&format!("SELECT id FROM {table} FOR UPDATE"), None)
        .await
        .unwrap_err();
    assert!(matches!(err, DbError::Query(_)), "{err:?}");

    let out = free
        .query(&format!("SELECT count(*) AS n FROM {table}"), None)
        .await
        .unwrap();
    assert_eq!(out.rows[0]["n"], json!(3));

    free.close().await;
    src.close().await;
    admin
        .execute(format!("DROP TABLE {table}").as_str())
        .await
        .unwrap();
    admin.close().await;
}
