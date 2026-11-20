//! R5 hand-off inventory: code-derived candidates are not release captures.
//! Missing release evidence is represented by `sql: None`, never empty SQL.
use crate::catalog::{self, Store};

pub const CREATING_VERSIONS: [&str; 4] = ["v1.80.0", "v1.80.1", "v1.81.0", "v1.82.0"];
pub const TARGET: &str = "v1.83.0";
pub const SOURCE: &str = "v1.82.x";

/// Authoritative hand-off slot for one creating-version/store pair. These are
/// intentionally unfilled: code candidates cannot prove released CREATE/ALTER
/// ordering. The later migration package must use these slots, not SETS alone.
#[derive(Debug, serde::Serialize)]
pub struct ReleaseCapture {
    pub id: String,
    pub creating_version: &'static str,
    pub last_version: &'static str,
    pub path: &'static str,
    pub expected_sql: Option<&'static str>,
    pub user_version: Option<i64>,
    pub application_id: Option<i64>,
}
impl ReleaseCapture {
    pub fn matches(&self, conn: &rusqlite::Connection) -> crate::Result<bool> {
        let (Some(sql), Some(user), Some(application)) = (self.expected_sql, self.user_version, self.application_id)
            else { return Ok(false); };
        let expected = rusqlite::Connection::open_in_memory()?;
        expected.execute_batch(sql)?;
        Ok(crate::sqlite::schema(conn)? == crate::sqlite::schema(&expected)?
            && conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? == user
            && conn.query_row("PRAGMA application_id", [], |r| r.get::<_, i64>(0))? == application)
    }
}
pub const STORES: [Store; 10] = [Store::Approvals, Store::Autopilot, Store::Cron,
    Store::GoogleChatInbox, Store::McpKeys, Store::Memory, Store::Sessions,
    Store::Tasks, Store::Users, Store::Workflow];

pub fn release_captures() -> Vec<ReleaseCapture> {
    let mut captures = Vec::new();
    for version in CREATING_VERSIONS {
        for store in STORES {
            captures.push(ReleaseCapture {
                id: format!("{}-{version}-to-v1.82.x-NEEDS-RELEASE-CAPTURE", store.file_name()),
                creating_version: version, last_version: SOURCE, path: store.file_name(),
                expected_sql: None, user_version: None, application_id: None,
            });
        }
    }
    captures.sort_by(|a, b| a.id.cmp(&b.id));
    captures
}

pub struct ExpectedSet {
    pub id: &'static str,
    pub store: Store,
    pub sql: Option<&'static str>,
    pub user_version: i64,
    pub provenance: &'static str,
}
impl ExpectedSet {
    /// A placeholder can never become a match (not even an empty database).
    pub fn matches(&self, conn: &rusqlite::Connection) -> crate::Result<bool> {
        let Some(sql) = self.sql else { return Ok(false); };
        let expected = rusqlite::Connection::open_in_memory()?;
        expected.execute_batch(sql)?;
        Ok(crate::sqlite::schema(conn)? == crate::sqlite::schema(&expected)?
            && conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? == self.user_version
            && conn.query_row("PRAGMA application_id", [], |r| r.get::<_, i64>(0))? == 0)
    }
}

// Existing byte-pinned SQL is reused rather than relabelled as captured output.
// Mapping versions remain V180/V181 where content columns did not change.
pub static SETS: &[ExpectedSet] = &[
    ExpectedSet { id: "sessions-code-fresh-to-v182", store: Store::Sessions, sql: Some(catalog::SESSION_V182), user_version: 0,
        provenance: "duduclaw-gateway/src/session.rs SessionManager; recipient_guard/origin.rs SCHEMA" },
    ExpectedSet { id: "sessions-code-upgraded-to-v182", store: Store::Sessions, sql: Some(catalog::SESSION_V182_UPGRADED), user_version: 0,
        provenance: "SessionManager ordered ALTER TABLE migrations; recipient_guard/origin.rs SCHEMA" },
    ExpectedSet { id: "sessions-code-fresh-channel-to-v182", store: Store::Sessions,
        sql: Some(concat!(include_str!("session-v181.sql"), include_str!("session-origin-proofs-v182.sql"), include_str!("channel-settings-v1793.sql"))), user_version: 0,
        provenance: "SessionManager + ChannelSettingsManager::from_session_db" },
    ExpectedSet { id: "sessions-code-upgraded-channel-to-v182", store: Store::Sessions,
        sql: Some(concat!(include_str!("session-v181-upgraded.sql"), include_str!("session-origin-proofs-v182.sql"), include_str!("channel-settings-v1793.sql"))), user_version: 0,
        provenance: "SessionManager ordered ALTER TABLE + ChannelSettingsManager::from_session_db" },
    ExpectedSet { id: "tasks-code-to-v182", store: Store::Tasks, sql: Some(catalog::TASKS_V182), user_version: 0,
        provenance: "duduclaw-gateway/src/task_store/schema.rs TaskStore initializer + source_origin_proof ALTER" },
    ExpectedSet { id: "cron-code-to-v182", store: Store::Cron, sql: Some(catalog::CRON_V182), user_version: 0,
        provenance: "duduclaw-gateway/src/cron_store.rs CronStore initializer, ordered TEXT column loop and scoped indexes" },
    ExpectedSet { id: "approvals-code-to-v182", store: Store::Approvals, sql: Some(catalog::APPROVALS_V181), user_version: 0,
        provenance: "duduclaw-gateway/src/approval/store.rs ApprovalStore initializer + lease and employee_incarnation migrations; historical lease label unverified" },
    ExpectedSet { id: "memory-code-without-causal-to-v182", store: Store::Memory, sql: Some(catalog::MEMORY_V180), user_version: 0,
        provenance: "duduclaw-memory/src/engine.rs SqliteMemoryEngine initializer; excludes optional causal store" },
    ExpectedSet { id: "workflow-code-to-v182", store: Store::Workflow, sql: Some(include_str!("workflow-v1793.sql")), user_version: 0,
        provenance: "duduclaw-gateway/src/workflow/store.rs and draft_store.rs initializers (unchanged v1.80-v1.82 DDL)" },
    ExpectedSet { id: "autopilot-code-to-v182", store: Store::Autopilot, sql: Some(include_str!("autopilot-v182.sql")), user_version: 0,
        provenance: "duduclaw-gateway/src/autopilot_store.rs AutopilotStore + autopilot_approval_dispatch initializer" },
    ExpectedSet { id: "mcp-keys-code-to-v182", store: Store::McpKeys, sql: Some(include_str!("mcp-keys-v182.sql")), user_version: 1,
        provenance: "duduclaw-core/src/mcp_key_store.rs SCHEMA_SQL" },
    ExpectedSet { id: "googlechat-inbox-code-to-v182", store: Store::GoogleChatInbox, sql: Some(include_str!("googlechat-inbox-v182.sql")), user_version: 0,
        provenance: "duduclaw-gateway/src/googlechat_inbox.rs inbox initializer; recovery inventory only, never replay" },
    ExpectedSet { id: "users-NEEDS-RELEASE-CAPTURE-to-v182", store: Store::Users, sql: None, user_version: 0,
        provenance: "duduclaw-auth/src/db.rs and mfa/remembered/isolation migrations: historical CREATE/ALTER ordering needs released homes; users proposals remain refused" },
    ExpectedSet { id: "memory-causal-NEEDS-RELEASE-CAPTURE-to-v182", store: Store::Memory, sql: None, user_version: 1,
        provenance: "duduclaw-memory/src/causal.rs initialize_schema; optional same-file causal tables and migration ordering need released homes" },
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expected_placeholders_never_match_empty_databases() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for set in SETS.iter().filter(|s| s.sql.is_none()) { assert!(!set.matches(&conn).unwrap()); }
        let captures = release_captures();
        assert_eq!(captures.len(), 40);
        for capture in captures { assert!(!capture.matches(&conn).unwrap()); }
    }
    #[test]
    fn expected_code_sets_exact_match_and_extra_index_refused() {
        for set in SETS.iter().filter(|s| s.sql.is_some()) {
            let conn = rusqlite::Connection::open_in_memory().unwrap();
            conn.execute_batch(set.sql.unwrap()).unwrap();
            assert!(set.matches(&conn).unwrap(), "{}", set.id);
            let table: String = conn.query_row("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND sql NOT LIKE 'CREATE VIRTUAL%' ORDER BY name LIMIT 1", [], |r| r.get(0)).unwrap();
            let column: String = conn.query_row("SELECT name FROM pragma_table_info(?1) ORDER BY cid LIMIT 1", [&table], |r| r.get(0)).unwrap();
            // Only trusted catalog identifiers reach this test-only statement.
            conn.execute_batch(&format!(r#"CREATE INDEX planted_idx ON "{table}"("{column}");"#)).unwrap();
            assert!(!set.matches(&conn).unwrap(), "{}", set.id);
        }
    }
}
