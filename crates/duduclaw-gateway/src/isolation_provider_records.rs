//! R8r-3a: the gateway's NON-SECRET record of the model connection documents
//! an Admin set through the dashboard (`isolation.provider_set` /
//! `isolation.provider_clear`, `handlers/isolation_providers_rpc.rs`).
//!
//! There is no read-back action for an employee's `providers.json` (root
//! never answers its contents), so `isolation.provider_status` can only say
//! what the gateway itself saw confirmed. Per employee this store keeps
//! exactly: each provider's `name`, `protocol` and endpoint origin
//! (`scheme://host[:port]`, never the path or query), the root's
//! `updated_unix`, and the employee incarnation the set was made for. Never a
//! key, a hash of a key, a full URL or the document.
//!
//! A row is written only after root CONFIRMED a set, deleted only after root
//! confirmed a clear; an unconfirmed outcome leaves it untouched. Documents
//! written by the host root command (`duduclaw service provider set`) never
//! pass through the gateway and are not visible here.
//!
//! Storage: `<home>/isolation-providers.db`, opened only through the hardened
//! private-database opener ([`crate::fs_safe::sqlite::open`]: no-follow
//! descriptor walk, private owner/mode checks on the file and its sidecars,
//! `SQLITE_OPEN_NOFOLLOW`), the same pattern as the connector catalog
//! (`isolation-connectors.db`). Each change is one `BEGIN IMMEDIATE`
//! transaction (SQLite's file lock + atomic commit). The name starts with
//! `isolation-`, so backup/restore and import treat it as isolation state
//! (`backup_restore_isolation`). It is created only behind the isolation gate
//! (the RPC refuses before any store is touched when isolation is inactive).
// The store is Unix-only (as the hardened opener and isolation itself).
#![cfg_attr(not(unix), allow(dead_code))]
use std::collections::BTreeMap;
use std::path::Path;

use duduclaw_isolation::ids::EmployeeId;

pub(crate) const RECORDS_FILE: &str = "isolation-providers.db";
/// Upper bound on rows read; more is treated as damage (refuse, never trim).
const MAX_ROWS: usize = 4096;
/// Upper bound on the stored summary text of one row.
const MAX_SUMMARY_BYTES: usize = 64 * 1024;
/// The one table, compared byte for byte with `sqlite_master` on every open.
const TABLE_SQL: &str = "CREATE TABLE provider_records(\
employee_hex TEXT PRIMARY KEY NOT NULL CHECK(length(employee_hex)=32),\
incarnation INTEGER NOT NULL CHECK(incarnation>=0),\
providers TEXT NOT NULL,\
updated_unix INTEGER NOT NULL CHECK(updated_unix>=0))";

/// One provider as status shows it. Nothing here is secret.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProviderSummary {
    pub(crate) name: String,
    /// `chat_completions` | `responses` | `anthropic` | `gemini`.
    pub(crate) protocol: String,
    /// `scheme://host[:port]` only.
    pub(crate) origin: String,
}

/// What the gateway recorded for one employee after a confirmed set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) incarnation: u32,
    pub(crate) providers: Vec<ProviderSummary>,
    pub(crate) updated_unix: u64,
}

const PROTOCOLS: [&str; 4] = ["chat_completions", "responses", "anthropic", "gemini"];

/// The shape every stored (and every written) summary must have.
fn valid(providers: &[ProviderSummary]) -> bool {
    !providers.is_empty() && providers.len() <= 32
        && providers.iter().all(|provider| {
            !provider.name.is_empty() && provider.name.len() <= 128 && !provider.name.chars().any(char::is_control)
                && PROTOCOLS.contains(&provider.protocol.as_str())
                && provider.origin.starts_with("https://") && provider.origin.len() <= 4096
                && !provider.origin.chars().any(|c| c.is_control() || matches!(c, '?' | '#' | '@'))
                && !provider.origin["https://".len()..].contains('/')
        })
        && {
            let mut names = std::collections::BTreeSet::new();
            providers.iter().all(|provider| names.insert(provider.name.as_str()))
        }
}

#[cfg(unix)]
fn open(home: &Path) -> Result<rusqlite::Connection, ()> {
    let mut connection = crate::fs_safe::sqlite::open(home, RECORDS_FILE).map_err(|_| ())?;
    connection.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| ())?;
    connection.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA synchronous=FULL;").map_err(|_| ())?;
    let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|_| ())?;
    let objects: Vec<(String, String, Option<String>)> = {
        let mut query = tx.prepare("SELECT type,name,sql FROM sqlite_master").map_err(|_| ())?;
        let rows = query.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(|_| ())?
            .collect::<Result<Vec<_>, _>>().map_err(|_| ())?;
        rows
    };
    // Exactly our table and its primary-key index, or nothing yet.
    let table = objects.iter().find(|(kind, name, _)| kind == "table" && name == "provider_records");
    let foreign = objects.iter().any(|(kind, name, _)| !(kind == "table" && name == "provider_records")
        && !(kind == "index" && name.starts_with("sqlite_autoindex_provider_records")));
    match (table, foreign) {
        (_, true) => return Err(()),
        (None, false) => { tx.execute_batch(TABLE_SQL).map_err(|_| ())?; }
        (Some((_, _, sql)), false) => if sql.as_deref() != Some(TABLE_SQL) { return Err(()); },
    }
    tx.commit().map_err(|_| ())?;
    Ok(connection)
}

/// Every recorded employee. No store yet (nothing was ever set through the
/// dashboard) is an empty map and creates nothing; an unsafe, foreign or
/// damaged store, an invalid row or more than [`MAX_ROWS`] rows is `Err`
/// (never a shortened answer).
pub(crate) fn read_all(home: &Path) -> Result<BTreeMap<EmployeeId, Record>, ()> {
    #[cfg(unix)]
    {
        match std::fs::symlink_metadata(home.join(RECORDS_FILE)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(_) => return Err(()),
            Ok(_) => {}
        }
        let connection = open(home)?;
        let bound = i64::try_from(MAX_ROWS + 1).map_err(|_| ())?;
        let mut query = connection.prepare(
            "SELECT employee_hex,incarnation,providers,updated_unix FROM provider_records ORDER BY employee_hex LIMIT ?1")
            .map_err(|_| ())?;
        let rows = query.query_map([bound], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?))).map_err(|_| ())?.collect::<Result<Vec<_>, _>>().map_err(|_| ())?;
        if rows.len() > MAX_ROWS { return Err(()); }
        let mut records = BTreeMap::new();
        for (hex_id, incarnation, providers, updated_unix) in rows {
            let employee = duduclaw_migration::allocation::providers::parse_employee_id(&hex_id).ok_or(())?;
            if providers.len() > MAX_SUMMARY_BYTES { return Err(()); }
            let providers: Vec<ProviderSummary> = serde_json::from_str(&providers).map_err(|_| ())?;
            if !valid(&providers) { return Err(()); }
            let record = Record { incarnation: u32::try_from(incarnation).map_err(|_| ())?, providers,
                updated_unix: u64::try_from(updated_unix).map_err(|_| ())? };
            if records.insert(employee, record).is_some() { return Err(()); }
        }
        Ok(records)
    }
    #[cfg(not(unix))]
    { let _ = home; Err(()) }
}

/// Record a confirmed set (replaces any earlier row of the employee).
pub(crate) fn put(home: &Path, employee: EmployeeId, record: &Record) -> Result<(), ()> {
    if !valid(&record.providers) { return Err(()); }
    let providers = serde_json::to_string(&record.providers).map_err(|_| ())?;
    if providers.len() > MAX_SUMMARY_BYTES { return Err(()); }
    #[cfg(unix)]
    {
        let mut connection = open(home)?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|_| ())?;
        tx.execute("INSERT INTO provider_records(employee_hex,incarnation,providers,updated_unix) VALUES(?1,?2,?3,?4)
            ON CONFLICT(employee_hex) DO UPDATE SET incarnation=excluded.incarnation,providers=excluded.providers,
            updated_unix=excluded.updated_unix",
            rusqlite::params![hex::encode(employee.0), i64::from(record.incarnation), providers,
                i64::try_from(record.updated_unix).map_err(|_| ())?]).map_err(|_| ())?;
        tx.commit().map_err(|_| ())
    }
    #[cfg(not(unix))]
    { let _ = (home, employee, providers); Err(()) }
}

/// Remove the row of a confirmed clear. Absent (store or row) is success.
pub(crate) fn remove(home: &Path, employee: EmployeeId) -> Result<(), ()> {
    #[cfg(unix)]
    {
        match std::fs::symlink_metadata(home.join(RECORDS_FILE)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(()),
            Ok(_) => {}
        }
        let mut connection = open(home)?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|_| ())?;
        tx.execute("DELETE FROM provider_records WHERE employee_hex=?1", [hex::encode(employee.0)]).map_err(|_| ())?;
        tx.commit().map_err(|_| ())
    }
    #[cfg(not(unix))]
    { let _ = (home, employee); Err(()) }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn summary(name: &str) -> ProviderSummary {
        ProviderSummary { name: name.into(), protocol: "chat_completions".into(), origin: "https://api.example.test".into() }
    }

    #[test]
    fn records_round_trip_and_remove() {
        let home = crate::fs_safe::private_test_home().unwrap();
        assert!(read_all(home.path()).unwrap().is_empty());
        assert!(!home.path().join(RECORDS_FILE).exists(), "a read never creates the store");
        assert!(remove(home.path(), EmployeeId([1; 16])).is_ok());
        let record = Record { incarnation: 2, providers: vec![summary("main"), summary("backup")], updated_unix: 1_700_000_000 };
        put(home.path(), EmployeeId([1; 16]), &record).unwrap();
        assert_eq!(read_all(home.path()).unwrap().get(&EmployeeId([1; 16])), Some(&record));
        let replaced = Record { incarnation: 2, providers: vec![summary("only")], updated_unix: 1_700_000_100 };
        put(home.path(), EmployeeId([1; 16]), &replaced).unwrap();
        assert_eq!(read_all(home.path()).unwrap().get(&EmployeeId([1; 16])), Some(&replaced));
        remove(home.path(), EmployeeId([1; 16])).unwrap();
        assert!(read_all(home.path()).unwrap().is_empty());
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.path().join(RECORDS_FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "private store");
    }

    #[test]
    fn records_refuse_anything_but_a_summary() {
        let home = crate::fs_safe::private_test_home().unwrap();
        for providers in [vec![], vec![summary("a"), summary("a")],
            vec![ProviderSummary { origin: "https://api.example.test/v1".into(), ..summary("a") }],
            vec![ProviderSummary { origin: "https://api.example.test?x=1".into(), ..summary("a") }],
            vec![ProviderSummary { origin: "http://api.example.test".into(), ..summary("a") }],
            vec![ProviderSummary { protocol: "other".into(), ..summary("a") }]] {
            assert!(put(home.path(), EmployeeId([1; 16]), &Record { incarnation: 0, providers, updated_unix: 1 }).is_err());
        }
        // A damaged row refuses the whole read.
        put(home.path(), EmployeeId([1; 16]), &Record { incarnation: 0, providers: vec![summary("a")], updated_unix: 1 }).unwrap();
        let connection = rusqlite::Connection::open(home.path().join(RECORDS_FILE)).unwrap();
        connection.execute("UPDATE provider_records SET providers='[{\"name\":\"a\",\"protocol\":\"chat_completions\",\"origin\":\"https://h\",\"api_key\":\"x\"}]'", []).unwrap();
        drop(connection);
        assert!(read_all(home.path()).is_err());
    }
}
