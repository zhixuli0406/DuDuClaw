//! End-to-end tests against a real (temp-file) SQLite database.
//!
//! SQLite is the one driver that needs no external service, so it carries the
//! behavioural coverage for the parts that only a live database can prove:
//! schema listing, the allowlist, truncation, value mapping, and the read-only
//! guards actually refusing writes.

use std::path::Path;

use duduclaw_db::{
    DbError, DbSource, DbSourceConfig, Driver, Filter, FilterOp, SelectRequest,
};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};

/// Create the fixture database and return its path.
async fn fixture(dir: &Path) -> String {
    let path = dir.join("demo.sqlite");
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let pool = SqlitePool::connect_with(opts).await.expect("create db");

    sqlx::query(
        "CREATE TABLE customers (\
            id INTEGER PRIMARY KEY, \
            name TEXT, \
            email TEXT, \
            phone TEXT, \
            balance REAL, \
            photo BLOB)",
    )
    .execute(&pool)
    .await
    .unwrap();
    // A second table deliberately left OUT of allowed_tables.
    sqlx::query("CREATE TABLE secrets (id INTEGER PRIMARY KEY, token TEXT)")
        .execute(&pool)
        .await
        .unwrap();

    for (id, name, email, phone, balance) in [
        (1, "王小明", "ming@example.com", "0912-345-678", 12.5_f64),
        (2, "Amy Chen", "amy@example.com", "0922-111-222", 0.0),
        (3, "Bob Lin", "bob@example.com", "0933-999-888", -7.25),
    ] {
        sqlx::query(
            "INSERT INTO customers (id, name, email, phone, balance, photo) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(name)
        .bind(email)
        .bind(phone)
        .bind(balance)
        .bind(b"\x01\x02".to_vec())
        .execute(&pool)
        .await
        .unwrap();
    }
    // One row with NULLs to exercise the null mapping.
    sqlx::query("INSERT INTO customers (id, name) VALUES (4, 'Null Person')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO secrets (id, token) VALUES (1, 'super-secret')")
        .execute(&pool)
        .await
        .unwrap();

    pool.close().await;
    path.to_string_lossy().to_string()
}

async fn open(dir: &Path, tables: &[&str]) -> DbSource {
    let url = fixture(dir).await;
    let cfg = DbSourceConfig::new(
        "demo",
        Driver::Sqlite,
        url,
        tables.iter().map(|s| s.to_string()).collect(),
    );
    DbSource::connect(cfg).await.expect("connect")
}

#[tokio::test]
async fn ping_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let src = open(dir.path(), &["customers"]).await;
    src.ping().await.expect("ping");
}

#[tokio::test]
async fn list_tables_honours_the_allowlist() {
    let dir = tempfile::tempdir().unwrap();
    let src = open(dir.path(), &["customers"]).await;
    let tables = src.list_tables().await.unwrap();
    assert_eq!(tables.len(), 1, "{tables:?}");
    assert_eq!(tables[0].name, "customers");
    let cols: Vec<&str> = tables[0].columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(cols, vec!["id", "name", "email", "phone", "balance", "photo"]);
    assert!(
        tables[0]
            .columns
            .iter()
            .any(|c| c.data_type.eq_ignore_ascii_case("TEXT")),
        "declared types should come back: {:?}",
        tables[0].columns
    );
}

#[tokio::test]
async fn list_tables_wildcard_sees_everything() {
    let dir = tempfile::tempdir().unwrap();
    let src = open(dir.path(), &["*"]).await;
    let names: Vec<String> = src
        .list_tables()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert!(names.contains(&"customers".to_string()));
    assert!(names.contains(&"secrets".to_string()));
}

#[tokio::test]
async fn select_returns_rows_with_value_mapping() {
    let dir = tempfile::tempdir().unwrap();
    let src = open(dir.path(), &["customers"]).await;
    let out = src
        .select(&SelectRequest {
            table: "customers".into(),
            filter: vec![Filter {
                column: "id".into(),
                op: FilterOp::Eq,
                value: json!(1),
            }],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(out.row_count, 1);
    assert!(!out.truncated);
    let row = &out.rows[0];
    assert_eq!(row["id"], json!(1));
    assert_eq!(row["name"], json!("王小明"));
    assert_eq!(row["balance"], json!(12.5));
    // BLOB → base64 of 0x01 0x02
    assert_eq!(row["photo"], json!("AQI="));
}

#[tokio::test]
async fn select_maps_sql_null_to_json_null() {
    let dir = tempfile::tempdir().unwrap();
    let src = open(dir.path(), &["customers"]).await;
    let out = src
        .select(&SelectRequest {
            table: "customers".into(),
            filter: vec![Filter {
                column: "id".into(),
                op: FilterOp::Eq,
                value: json!(4),
            }],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(out.rows[0]["email"], serde_json::Value::Null);
    assert_eq!(out.rows[0]["balance"], serde_json::Value::Null);
}

#[tokio::test]
async fn select_projection_order_and_limit() {
    let dir = tempfile::tempdir().unwrap();
    let src = open(dir.path(), &["customers"]).await;
    let out = src
        .select(&SelectRequest {
            table: "customers".into(),
            columns: vec!["id".into(), "name".into()],
            order_by: Some("id desc".into()),
            limit: Some(2),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(out.row_count, 2);
    assert!(out.truncated, "4 rows exist, 2 requested");
    assert_eq!(out.rows[0]["id"], json!(4));
    assert_eq!(out.rows[0].len(), 2, "projection limits the columns");
}

#[tokio::test]
async fn select_in_and_like_filters() {
    let dir = tempfile::tempdir().unwrap();
    let src = open(dir.path(), &["customers"]).await;
    let out = src
        .select(&SelectRequest {
            table: "customers".into(),
            filter: vec![Filter {
                column: "id".into(),
                op: FilterOp::In,
                value: json!([1, 3]),
            }],
            order_by: Some("id".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(out.row_count, 2);
    assert_eq!(out.rows[0]["id"], json!(1));
    assert_eq!(out.rows[1]["id"], json!(3));

    let out = src
        .select(&SelectRequest {
            table: "customers".into(),
            filter: vec![Filter {
                column: "email".into(),
                op: FilterOp::Like,
                value: json!("amy%"),
            }],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(out.row_count, 1);
    assert_eq!(out.rows[0]["name"], json!("Amy Chen"));
}

#[tokio::test]
async fn select_truncates_at_max_rows() {
    let dir = tempfile::tempdir().unwrap();
    let url = fixture(dir.path()).await;
    let cfg = DbSourceConfig::new("demo", Driver::Sqlite, url, vec!["customers".into()])
        .with_max_rows(2);
    let src = DbSource::connect(cfg).await.unwrap();
    let out = src
        .select(&SelectRequest {
            table: "customers".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(out.row_count, 2);
    assert!(out.truncated);

    // A caller asking for more than max_rows still gets max_rows.
    let out = src
        .select(&SelectRequest {
            table: "customers".into(),
            limit: Some(100),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(out.row_count, 2);
    assert!(out.truncated);
}

#[tokio::test]
async fn select_rejects_unlisted_table_and_bad_identifiers() {
    let dir = tempfile::tempdir().unwrap();
    let src = open(dir.path(), &["customers"]).await;

    let err = src
        .select(&SelectRequest {
            table: "secrets".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(matches!(err, DbError::TableNotAllowed { .. }), "{err:?}");

    let err = src
        .select(&SelectRequest {
            table: "customers\"; DROP TABLE customers; --".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(matches!(err, DbError::InvalidIdentifier(_)), "{err:?}");

    let err = src
        .select(&SelectRequest {
            table: "customers".into(),
            columns: vec!["name); DELETE FROM customers; --".into()],
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(matches!(err, DbError::InvalidIdentifier(_)), "{err:?}");
}

#[tokio::test]
async fn select_filter_value_is_bound_not_interpolated() {
    let dir = tempfile::tempdir().unwrap();
    let src = open(dir.path(), &["customers"]).await;
    // A classic injection payload as a *value* must be treated as data: it
    // simply matches nothing, and the table is still there afterwards.
    let out = src
        .select(&SelectRequest {
            table: "customers".into(),
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

    let still_there = src
        .select(&SelectRequest {
            table: "customers".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(still_there.row_count, 4);
}

#[tokio::test]
async fn query_accepts_select_and_with() {
    let dir = tempfile::tempdir().unwrap();
    // Free-form SQL requires a wildcard source (see `query_refused_when_the_source_has_an_allowlist`).
    let src = open(dir.path(), &["*"]).await;

    let out = src
        .query("SELECT id, name FROM customers ORDER BY id", None)
        .await
        .unwrap();
    assert_eq!(out.row_count, 4);
    assert_eq!(out.rows[0]["name"], json!("王小明"));

    let out = src
        .query(
            "WITH recent AS (SELECT * FROM customers WHERE id > 2) SELECT count(*) AS n FROM recent",
            None,
        )
        .await
        .unwrap();
    assert_eq!(out.rows[0]["n"], json!(2));
}

#[tokio::test]
async fn query_refuses_writes_and_multi_statements() {
    let dir = tempfile::tempdir().unwrap();
    let src = open(dir.path(), &["*"]).await;

    for sql in [
        "DELETE FROM customers",
        "UPDATE customers SET name = 'x'",
        "INSERT INTO customers (id) VALUES (99)",
        "DROP TABLE customers",
        "SELECT 1; DELETE FROM customers",
        "SELECT 1;",
        "--x\nDELETE FROM customers",
        "/* hi */ UPDATE customers SET name = 'x'",
    ] {
        let err = src.query(sql, None).await.unwrap_err();
        assert!(
            matches!(err, DbError::StatementRejected(_)),
            "{sql:?} should be refused by the statement guard, got {err:?}"
        );
    }

    // Nothing was executed.
    let out = src
        .select(&SelectRequest {
            table: "customers".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(out.row_count, 4);
}

#[tokio::test]
async fn query_respects_the_row_cap() {
    let dir = tempfile::tempdir().unwrap();
    let url = fixture(dir.path()).await;
    let cfg = DbSourceConfig::new("demo", Driver::Sqlite, url, vec!["*".into()])
        .with_max_rows(2);
    let src = DbSource::connect(cfg).await.unwrap();
    let out = src.query("SELECT * FROM customers", None).await.unwrap();
    assert_eq!(out.row_count, 2);
    assert!(out.truncated);
}

/// The two sides of the free-SQL rule.
///
/// An allowlist cannot be enforced against arbitrary SQL, so a source that has
/// one refuses `db_query` outright — including the statement that would have
/// read straight past the allowlist. `["*"]` is the operator opting in.
#[tokio::test]
async fn query_refused_when_the_source_has_an_allowlist() {
    let dir = tempfile::tempdir().unwrap();
    let src = open(dir.path(), &["customers"]).await;

    // Reading an unlisted table — the case the rule exists for.
    let err = src.query("SELECT token FROM secrets", None).await.unwrap_err();
    assert!(matches!(err, DbError::FreeSqlNotAllowed { .. }), "{err:?}");
    let msg = err.to_string();
    assert!(msg.contains("設有資料表白名單"), "{msg}");
    assert!(msg.contains("db_select"), "{msg}");
    assert!(msg.contains("[\"*\"]"), "{msg}");

    // Even a query confined to an allowed table is refused: the rule is about
    // the source, not about this particular statement (no SQL parsing).
    let err = src.query("SELECT id FROM customers", None).await.unwrap_err();
    assert!(matches!(err, DbError::FreeSqlNotAllowed { .. }), "{err:?}");

    // The allowlist check runs BEFORE the statement guard, so a write attempt
    // on a restricted source reports the access decision, not the syntax one.
    let err = src.query("DELETE FROM customers", None).await.unwrap_err();
    assert!(matches!(err, DbError::FreeSqlNotAllowed { .. }), "{err:?}");
}

#[tokio::test]
async fn query_runs_on_a_wildcard_source() {
    let dir = tempfile::tempdir().unwrap();
    let src = open(dir.path(), &["*"]).await;
    let out = src.query("SELECT token FROM secrets", None).await.unwrap();
    assert_eq!(out.row_count, 1);
    assert_eq!(out.rows[0]["token"], json!("super-secret"));
}

#[tokio::test]
async fn connect_fails_clearly_on_a_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = DbSourceConfig::new(
        "demo",
        Driver::Sqlite,
        dir.path().join("nope.sqlite").to_string_lossy().to_string(),
        vec!["*".into()],
    );
    let err = DbSource::connect(cfg).await.unwrap_err();
    assert!(matches!(err, DbError::Connect(_)), "{err:?}");
    // The base name helps; the directory path is not echoed back.
    let msg = err.to_string();
    assert!(msg.contains("nope.sqlite"), "{msg}");
}
