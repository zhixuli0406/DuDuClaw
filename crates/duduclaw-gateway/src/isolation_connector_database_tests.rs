use super::*;
use std::sync::{Mutex, atomic::{AtomicBool, Ordering}};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, EmployeeBinding,
    Lane, LiveRequestVerifier, Principal}, ids::{EmployeeId, RunId, Sha256Digest}};
use duduclaw_mcp_boundary::{bindings::BindingStore, tool_policy::{authorize, ResourceResolver, ToolPolicy}};
use crate::isolation_connectors::{ConnectorCatalog, ConnectorKind};
/// Catalog account ids (production format) shared with the binding double.
const ALICE_DB: &str = concat!("a11ce000", "00000000", "00000000", "000000d1");
const ALICE_OTHER: &str = concat!("a11ce000", "00000000", "00000000", "000000d2");
struct Live(Arc<AtomicBool>);
impl LiveRequestVerifier for Live {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> {
        if self.0.load(Ordering::SeqCst) { Ok(()) } else { Err(VerificationError::Revoked) }
    }
}
fn text<const N: usize>(value: &str) -> Text<N> { Text::new(value.into()).unwrap() }
struct Resolver(Vec<ResourceGrant>);
impl ResourceResolver for Resolver {
    fn resolve(&self, _: &RequestContext, _: &ToolPolicy, _: &Value) -> Result<Vec<ResourceGrant>, VerificationError> {
        Ok(self.0.clone())
    }
}
struct Fixture {
    home: tempfile::TempDir, provider: Arc<DatabaseConnectorProvider>, context: RequestContext,
    resolver: Resolver, bindings: Arc<Mutex<BindingStore>>, catalog: Arc<ConnectorCatalog>, live: Arc<AtomicBool>,
}
impl Fixture {
    fn new(employee_scope: bool) -> Self {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/employee-a")).unwrap();
        std::fs::write(home.path().join("agents/employee-a/agent.toml"), "[capabilities]\ndb_sources=['demo']\n").unwrap();
        let db_path = home.path().join("demo.sqlite");
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch("CREATE TABLE customers(id INTEGER,name TEXT); INSERT INTO customers VALUES(1,'example'); CREATE TABLE secret(value TEXT);").unwrap();
        drop(db);
        std::fs::write(home.path().join("config.toml"), format!("[db_sources.demo]\ndriver='sqlite'\nurl={:?}\nallowed_tables=['*']\n", db_path.to_str().unwrap())).unwrap();
        let employee = EmployeeId([2;16]);
        let binding = EmployeeBinding::Personal(text("alice"));
        let principal = Principal::Human { user_id:text("alice"), generation:1 };
        let mut bindings = BindingStore::new(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
        bindings.register_principal(&principal).unwrap(); bindings.create_employee(employee, &binding).unwrap();
        bindings.register_connector(ALICE_DB, &binding).unwrap();
        let catalog = Arc::new(ConnectorCatalog::new(rusqlite::Connection::open_in_memory().unwrap()).unwrap());
        catalog.register_fixture(ALICE_DB, "database", ConnectorKind::Personal, &binding).unwrap();
        let resources = vec![ResourceGrant::Employee { id:employee }, ResourceGrant::Connector { account:text(ALICE_DB), binding:binding.clone() },
            ResourceGrant::DatabaseTable { database:text("demo"), table:text("customers") }];
        let live = Arc::new(AtomicBool::new(true));
        let context = RequestContext::with_live_verifier(CapabilityRecord {
            id:CapabilityId(Sha256Digest([1;32])), employee, run:RunId([3;16]), task:None, principal, employee_binding:binding,
            lane:Lane::Standard, tools:List::new(DATABASE_TOOLS.iter().map(|tool| text(tool)).collect()).unwrap(),
            resources:List::new(if employee_scope { vec![ResourceGrant::EmployeeScope { id:employee }] } else { resources.clone() }).unwrap(),
            policy_revision:1, gateway_epoch:[0;16], issued_at:1, not_before:1, expires_at:u64::MAX, nonce:[0;32], jti:[0;16],
        }, Arc::new(Live(live.clone())));
        let provider = Arc::new(DatabaseConnectorProvider::new(home.path().into(), "employee-a".into(), employee,
            vec![(ProductConnectorAccount::Personal { account:ALICE_DB.into(), owner:"alice".into() }, "demo".into())]).unwrap());
        Self { home, provider, context, resolver:Resolver(resources), bindings:Arc::new(Mutex::new(bindings)), catalog, live }
    }
    fn call(&self, tool: &str, args: Value) -> (SelectedConnector, AuthorizedToolCall) {
        let selected = SelectedConnector::select_with_policy(&self.context, tool, &args, self.bindings.clone(), self.catalog.clone(), Some(self.provider.clone())).unwrap();
        let call = authorize(&self.context, tool, &args, &self.resolver).unwrap();
        (selected, call)
    }
}
#[tokio::test]
async fn database_adapter_reads_exact_source_and_limits_finite_table_metadata() {
    let fixture = Fixture::new(false);
    let (selected, call) = fixture.call("db_select", json!({"source":"demo","table":"customers"}));
    let result = fixture.provider.execute(selected, call).await.unwrap().to_string();
    assert!(result.contains("example"));
    let (selected, call) = fixture.call("db_tables", json!({"source":"demo"}));
    let result = fixture.provider.execute(selected, call).await.unwrap().to_string();
    assert!(result.contains("customers")); assert!(!result.contains("secret"));
    let (selected, call) = fixture.call("db_query", json!({"source":"demo","sql":"SELECT * FROM secret"}));
    assert!(fixture.provider.execute(selected, call).await.is_err());
}
#[tokio::test]
async fn database_adapter_employee_scope_preserves_read_only_free_sql() {
    let fixture = Fixture::new(true);
    let (selected, call) = fixture.call("db_query", json!({"source":"demo","sql":"SELECT * FROM customers"}));
    assert!(fixture.provider.execute(selected, call).await.unwrap().to_string().contains("example"));
    let (selected, call) = fixture.call("db_query", json!({"source":"demo","sql":"DELETE FROM customers"}));
    assert!(fixture.provider.execute(selected, call).await.is_err());
}
#[tokio::test]
async fn database_adapter_fences_revocation_source_changes_and_agent_grant_removal() {
    for change in ["run", "source", "grant"] {
        let fixture = Fixture::new(false);
        let (selected, call) = fixture.call("db_select", json!({"source":"demo","table":"customers"}));
        match change {
            "run" => fixture.live.store(false, Ordering::SeqCst),
            "source" => std::fs::write(fixture.home.path().join("config.toml"), "[db_sources.demo]\ndriver='sqlite'\nurl='/tmp/changed.sqlite'\nallowed_tables=['*']\n").unwrap(),
            _ => std::fs::write(fixture.home.path().join("agents/employee-a/agent.toml"), "[capabilities]\ndb_sources=[]\n").unwrap(),
        }
        assert!(fixture.provider.execute(selected, call).await.is_err(), "{change}");
    }
}

#[tokio::test]
async fn database_source_argument_selects_among_multiple_product_accounts() {
    let mut fixture = Fixture::new(true);
    let config_path = fixture.home.path().join("config.toml");
    let mut config = std::fs::read_to_string(&config_path).unwrap();
    config.push_str("\n[db_sources.other]\ndriver='sqlite'\nurl='/tmp/not-opened.sqlite'\nallowed_tables=['*']\n");
    std::fs::write(&config_path, config).unwrap();
    std::fs::write(fixture.home.path().join("agents/employee-a/agent.toml"), "[capabilities]\ndb_sources=['demo','other']\n").unwrap();
    fixture.bindings.lock().unwrap().register_connector(ALICE_OTHER, &fixture.context.record().employee_binding).unwrap();
    fixture.catalog.register_fixture(ALICE_OTHER, "database", ConnectorKind::Personal,
        &fixture.context.record().employee_binding).unwrap();
    fixture.provider = Arc::new(DatabaseConnectorProvider::new(fixture.home.path().into(), "employee-a".into(), fixture.context.record().employee,
        vec![(ProductConnectorAccount::Personal { account:ALICE_DB.into(), owner:"alice".into() }, "demo".into()),
            (ProductConnectorAccount::Personal { account:ALICE_OTHER.into(), owner:"alice".into() }, "other".into())]).unwrap());
    let (selected, call) = fixture.call("db_select", json!({"source":"demo","table":"customers"}));
    assert_eq!(selected.account(), ALICE_DB);
    assert!(fixture.provider.execute(selected, call).await.unwrap().to_string().contains("example"));
    for (source, account) in [("demo",ALICE_DB), ("other",ALICE_OTHER)] {
        let args = json!({"source":source});
        let selected = SelectedConnector::select_with_policy(&fixture.context, "db_tables", &args, fixture.bindings.clone(),
            fixture.catalog.clone(), Some(fixture.provider.clone())).unwrap();
        assert_eq!(selected.account(), account);
        selected.recheck().unwrap();
    }
    assert!(SelectedConnector::select_with_policy(&fixture.context, "db_tables", &json!({"source":"ungranted"}),
        fixture.bindings.clone(), fixture.catalog.clone(), Some(fixture.provider.clone())).is_err());
}

#[tokio::test]
async fn database_table_columns_keep_the_public_type_field() {
    let fixture = Fixture::new(false);
    let (selected, call) = fixture.call("db_tables", json!({"source":"demo"}));
    let result = fixture.provider.execute(selected, call).await.unwrap();
    let value: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(value["tables"][0]["columns"][0]["type"], "INTEGER");
    assert!(value["tables"][0]["columns"][0].get("data_type").is_none());
}

#[tokio::test]
async fn source_listing_is_bounded_and_never_resolves_database_credentials() {
    let fixture = Fixture::new(false);
    let (selected, call) = fixture.call("db_sources", json!({}));
    let out = fixture.provider.execute(selected, call).await.unwrap();
    let rows: Value = serde_json::from_str(out["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(rows, json!([{"name":"demo","label":"demo","driver":"sqlite"}]));
    assert!(!out.to_string().contains("url"));
    let (selected, call) = fixture.call("db_sources", json!({}));
    fixture.live.store(false, Ordering::SeqCst);
    assert!(fixture.provider.execute(selected, call).await.is_err());
}

#[test]
fn database_live_policy_reads_ephemeral_scaffold_without_registry_fallback() {
    let fixture = Fixture::new(false);
    let ephemeral = fixture.home.path().join("agents/.ephemeral/eph-example");
    std::fs::create_dir_all(&ephemeral).unwrap();
    std::fs::write(ephemeral.join("agent.toml"), "[capabilities]\ndb_sources=['demo']\n").unwrap();
    assert!(source_config(fixture.home.path(), "eph-example", "demo").is_ok());
    std::fs::remove_file(ephemeral.join("agent.toml")).unwrap();
    let impostor = fixture.home.path().join("agents/eph-example");
    std::fs::create_dir_all(&impostor).unwrap();
    std::fs::write(impostor.join("agent.toml"), "[capabilities]\ndb_sources=['demo']\n").unwrap();
    assert!(source_config(fixture.home.path(), "eph-example", "demo").is_err());
}

#[tokio::test]
async fn finite_sql_lowers_to_a_single_exact_granted_table_and_keeps_literals_bound() {
    let fixture=Fixture::new(false);
    for sql in ["SELECT id,name FROM customers WHERE id=1 ORDER BY id DESC LIMIT 1", "SELECT * FROM \"customers\" WHERE name='example'", "SELECT * FROM customers WHERE name='x'' OR 1=1;--'"] {
        let(selected,call)=fixture.call("db_query",json!({"source":"demo","sql":sql}));
        let out=fixture.provider.execute(selected,call).await.unwrap();
        let payload:Value=serde_json::from_str(out["content"][0]["text"].as_str().unwrap()).unwrap();
        assert!(payload["row_count"].as_u64().unwrap()<=1);
        assert!(!out.to_string().contains("secret"));
    }
    for sql in ["SELECT * FROM secret","SELECT * FROM CUSTOMERS", "SELECT * FROM customers UNION SELECT * FROM secret",
        "WITH x AS (SELECT * FROM secret) SELECT * FROM x", "SELECT * FROM customers WHERE id IN (SELECT id FROM secret)",
        "SELECT * FROM customers; SELECT * FROM secret", "SELECT readfile('secret') FROM customers"] {
        let(selected,call)=fixture.call("db_query",json!({"source":"demo","sql":sql}));
        assert!(fixture.provider.execute(selected,call).await.is_err(),"{sql}");
    }
    let(selected,call)=fixture.call("db_query",json!({"source":"demo","sql":"SELECT * FROM customers","limit":1}));
    fixture.live.store(false,Ordering::SeqCst);assert!(fixture.provider.execute(selected,call).await.is_err());
}

#[test]
fn pre_connect_fence_refuses_backslash_escape_string_and_dollar_quoting() {
    for sql in [
        "SELECT 'a\\'' INTO OUTFILE '/tmp/never-write'",          // MySQL backslash bypass
        "SELECT 'x\\' FROM customers",
        "SELECT `a\\` FROM customers",
        "SELECT E'a\\'' FROM customers", "SELECT E'' FROM customers", // PostgreSQL E'…'
        "SELECT U&'d\\0061t' FROM customers",
        "SELECT $$ x $$ FROM customers", "SELECT $q$ x $q$ FROM customers", // dollar quoting
    ] {
        assert!(matches!(select_only_before_connect(sql), Err(VerificationError::Invalid)), "{sql}");
    }
    for sql in ["SELECT 'it''s' FROM customers", "SELECT 'INTO' FROM customers",
        "SELECT name FROM customers WHERE name = 'o''brien'", "SELECT \"a\"\"b\" FROM customers",
        "SELECT e FROM customers", "SELECT u & 1 FROM customers"] {
        assert!(select_only_before_connect(sql).is_ok(), "{sql}");
    }
}

#[tokio::test]
async fn database_policy_select_only_and_redacted_errors() {
    for employee_scope in [false, true] {
        let fixture = Fixture::new(employee_scope);
        // A missing SQLite file is a connection sentinel: policy failures must
        // be Invalid, not the Unavailable returned by an attempted connection.
        std::fs::remove_file(fixture.home.path().join("demo.sqlite")).unwrap();
        for sql in [
            "DELETE FROM customers", "UPDATE customers SET name='x'",
            "INSERT INTO customers VALUES (2,'x')", "DROP TABLE customers",
            "WITH x AS (DELETE FROM customers RETURNING *) SELECT * FROM x",
            "WITH x AS (SELECT 1) UPDATE customers SET name='x'",
            "SELECT * FROM customers; DELETE FROM customers",
            "SELECT * FROM customers; SELECT * FROM customers",
            "/* SELECT 1 */ DELETE FROM customers", "-- SELECT 1\nDELETE FROM customers",
            "/* outer /* nested */ comment */ UPDATE customers SET name='x'",
            "SELECT 1 /*! INTO OUTFILE '/tmp/never-write' */",
            "SELECT * FROM customers /* unterminated",
            "SELECT 1 /* outer /* inner */ ; DELETE FROM customers */",
            "SELECT 1 /*M! INTO OUTFILE '/tmp/never-write' */",
        ] {
            let (selected, call) = fixture.call("db_query", json!({"source":"demo","sql":sql}));
            assert!(matches!(fixture.provider.execute(selected, call).await,
                Err(VerificationError::Invalid)), "scope={employee_scope}, sql={sql}");
        }
    }
    let fixture = Fixture::new(true);
    std::fs::remove_file(fixture.home.path().join("demo.sqlite")).unwrap();
    for sql in [
        // MySQL backslash escape hides INTO from a ''-only scanner.
        "SELECT 'a\\'' INTO OUTFILE '/tmp/never-write'",
        "SELECT \"a\\\"\" INTO OUTFILE '/tmp/never-write'",
        "SELECT 'a\\\\' FROM customers",
        // PostgreSQL escape strings and Unicode escapes.
        "SELECT E'a\\'' FROM customers", "SELECT e'plain' FROM customers",
        "SELECT U&'d\\0061t' FROM customers",
        // PostgreSQL dollar quoting.
        "SELECT $$ x $$ FROM customers", "SELECT $tag$ ' $tag$ INTO x FROM customers",
        "SELECT $_t1$x$_t1$ FROM customers",
    ] {
        let (selected, call) = fixture.call("db_query", json!({"source":"demo","sql":sql}));
        assert!(matches!(fixture.provider.execute(selected, call).await,
            Err(VerificationError::Invalid)), "escape form must be refused: {sql}");
    }
    let fixture = Fixture::new(true);
    for sql in ["SELECT * FROM customers", "/* ordinary comment */ SELECT * FROM customers",
        "SELECT 'it''s' FROM customers", "SELECT name FROM customers WHERE name = 'o''brien'",
        "SELECT \"name\" FROM customers WHERE name <> 'a''b''c'",
        "WITH x AS (SELECT * FROM customers) SELECT * FROM x",
        "SELECT 'DELETE; UPDATE /* not a comment */' FROM customers"] {
        let (selected, call) = fixture.call("db_query", json!({"source":"demo","sql":sql}));
        assert!(fixture.provider.execute(selected, call).await.is_ok(), "{sql}");
    }
    // Force a real query error containing secret-shaped text in an identifier.
    let canary = "postgres://PRIVATE-USER:PRIVATE-PASSWORD@PRIVATE-HOST/PRIVATE-DSN";
    let (selected, call) = fixture.call("db_query", json!({"source":"demo",
        "sql":format!("SELECT * FROM \"{canary}\"")}));
    let error = fixture.provider.execute(selected, call).await.unwrap_err();
    assert!(matches!(error, VerificationError::Unavailable));
    let returned = format!("{error} {error:?}");
    for secret in [canary, "PRIVATE-USER", "PRIVATE-PASSWORD", "PRIVATE-HOST", "PRIVATE-DSN"] {
        assert!(!returned.contains(secret));
    }
    // The same boundary also hides connection errors and the actual source URL.
    let path = fixture.home.path().join("demo.sqlite");
    std::fs::remove_file(&path).unwrap();
    let (selected, call) = fixture.call("db_query", json!({"source":"demo","sql":"SELECT * FROM customers"}));
    let error = fixture.provider.execute(selected, call).await.unwrap_err();
    assert!(matches!(error, VerificationError::Unavailable));
    assert!(!format!("{error} {error:?}").contains(path.to_str().unwrap()));
}
