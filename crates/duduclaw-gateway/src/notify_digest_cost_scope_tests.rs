//! Home-scoped digest costs with process-global telemetry deliberately elsewhere.
use super::*;
use crate::cost_telemetry::{CostTelemetry, RequestType, TokenUsage};
use std::path::Path;

async fn seed_stored_cost(home: &Path, telemetry: &CostTelemetry, cost: u64) {
    telemetry
        .record(
            "owned-digest-agent",
            RequestType::Chat,
            "claude-sonnet-4-6",
            &TokenUsage {
                input_tokens: 100,
                ..TokenUsage::default()
            },
        )
        .await;
    // This is a ledger-reader regression, independent of catalog prices.
    // Use the real writer/schema, then pin the sole owned row's stored cost.
    let conn = rusqlite::Connection::open(home.join("cost_telemetry.db")).unwrap();
    assert_eq!(
        conn.execute(
            "UPDATE token_usage SET cost_millicents = ?1",
            rusqlite::params![cost],
        )
        .unwrap(),
        1
    );
    assert_eq!(
        telemetry
            .summary_global(24)
            .await
            .unwrap()
            .total_cost_millicents,
        cost
    );
}

async fn assert_home_scoped_costs() {
    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("global-a");
    let b = root.path().join("資料乙");
    let bad = root.path().join("bad-existing");
    for home in [&a, &b, &bad] {
        std::fs::create_dir(home).unwrap();
    }
    crate::cost_telemetry::init_telemetry(&a).unwrap();
    let global_a = crate::cost_telemetry::get_telemetry().unwrap();
    seed_stored_cost(&a, global_a, 17).await;
    let now = Utc::now();
    let since = now - chrono::Duration::hours(24);

    assert_eq!(collect(&a, since, now).await.cost_millicents, 17);
    let missing_cost = b.join("cost_telemetry.db");
    assert!(!missing_cost.exists());
    assert!(collect(&b, since, now).await.is_empty());
    assert!(
        !missing_cost.exists(),
        "digest reads must not create an absent cost database"
    );

    let own_b = CostTelemetry::new(&missing_cost).unwrap();
    seed_stored_cost(&b, &own_b, 43).await;
    assert_eq!(collect(&b, since, now).await.cost_millicents, 43);
    assert_eq!(collect(&a, since, now).await.cost_millicents, 17);
    assert_eq!(
        crate::cost_telemetry::get_telemetry()
            .unwrap()
            .summary_global(24)
            .await
            .unwrap()
            .total_cost_millicents,
        17,
        "home-scoped reads must not replace or reset the global instance"
    );

    let bad_path = bad.join("cost_telemetry.db");
    let original = b"owned malformed ledger";
    std::fs::write(&bad_path, original).unwrap();
    assert_eq!(collect(&bad, since, now).await.cost_millicents, 0);
    assert_eq!(std::fs::read(&bad_path).unwrap(), original);
}

#[test]
fn digest_costs_stay_with_the_requested_home_when_global_telemetry_is_elsewhere() {
    const CHILD: &str = "DUDU_TEST_DIGEST_COST_SCOPE_CHILD";
    if let Some(flag) = std::env::var_os(CHILD) {
        assert_eq!(flag, std::ffi::OsStr::new("isolated"));
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(assert_home_scoped_costs());
        return;
    }

    // The global singleton is exercised only in this fresh child. No parent
    // environment, singleton, real provider, or parallel test is changed.
    let owned = tempfile::tempdir().unwrap();
    let home = owned.path().join("home");
    let tmp = owned.path().join("tmp");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&tmp).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for dir in [&home, &tmp] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    let stdout_path = owned.path().join("stdout.log");
    let stderr_path = owned.path().join("stderr.log");
    let full_name = concat!(
        module_path!(),
        "::digest_costs_stay_with_the_requested_home_when_global_telemetry_is_elsewhere"
    );
    // libtest names omit the crate prefix that module_path! includes.
    let test_name = full_name.split_once("::").unwrap().1;
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("TMPDIR", &tmp)
        .env("TMP", &tmp)
        .env("TEMP", &tmp)
        .env(CHILD, "isolated")
        .current_dir(&home)
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(&stdout_path).unwrap())
        .stderr(std::fs::File::create(&stderr_path).unwrap());
    #[cfg(windows)]
    for key in ["SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let mut child = command.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            // try_wait returned None: the owned PID has not been reaped and
            // remains reserved. This child branch starts no descendants.
            timed_out = true;
            child.kill().unwrap();
            break child.wait().unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let stdout = std::fs::read_to_string(stdout_path).unwrap();
    let stderr = std::fs::read_to_string(stderr_path).unwrap();
    assert!(
        !timed_out,
        "isolated digest scope exceeded 20s: {stdout} {stderr}"
    );
    assert!(
        stdout.lines().any(|line| line == "running 1 test"),
        "the child must execute exactly its owned case: {stdout} {stderr}"
    );
    assert!(
        status.success(),
        "isolated digest scope failed: {stdout} {stderr}"
    );
}

#[tokio::test]
async fn existing_summary_does_not_create_missing_or_initialize_invalid_schema() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing-parent/cost_telemetry.db");
    assert!(
        CostTelemetry::summary_existing(&missing, 24)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!missing.parent().unwrap().exists());

    let blank = root.path().join("blank.db");
    drop(rusqlite::Connection::open(&blank).unwrap());
    let before = std::fs::read(&blank).unwrap();
    assert!(CostTelemetry::summary_existing(&blank, 24).await.is_err());
    assert_eq!(std::fs::read(&blank).unwrap(), before);
    let conn = rusqlite::Connection::open(&blank).unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| {
            row.get::<_, u64>(0)
        })
        .unwrap(),
        0
    );

    let bad = root.path().join("malformed.db");
    std::fs::write(&bad, b"not a sqlite ledger").unwrap();
    assert!(CostTelemetry::summary_existing(&bad, 24).await.is_err());
    assert_eq!(std::fs::read(&bad).unwrap(), b"not a sqlite ledger");
}

#[tokio::test]
async fn existing_summary_reads_a_compatible_ledger_without_migrating_its_schema() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("legacy.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE token_usage (
            input_tokens INTEGER, cache_read_tokens INTEGER,
            cache_creation_tokens INTEGER, output_tokens INTEGER,
            cache_efficiency REAL, cost_millicents INTEGER,
            cache_hit_rate REAL, cache_savings_millicents INTEGER,
            created_at TEXT
        );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO token_usage VALUES (100, 20, 0, 10, 0.2, 29, 0.2, 3, ?1)",
        rusqlite::params![Utc::now().to_rfc3339()],
    )
    .unwrap();
    let schema: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='token_usage'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    drop(conn);
    let bytes = std::fs::read(&path).unwrap();

    let summary = CostTelemetry::summary_existing(&path, 24)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.total_requests, 1);
    assert_eq!(summary.total_input_tokens, 100);
    assert_eq!(summary.total_cost_millicents, 29);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let conn = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row::<String, _, _>(
            "SELECT sql FROM sqlite_master WHERE name='token_usage'",
            [],
            |row| row.get(0),
        )
        .unwrap(),
        schema
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| {
            row.get::<_, u64>(0)
        })
        .unwrap(),
        1
    );
}
