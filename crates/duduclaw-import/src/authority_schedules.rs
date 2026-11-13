//! Inventory scheduling authority per record. Old workflow grants, execution
//! leases, delivery routes and action payloads never authorize fresh execution.
use super::*;

fn propose(builder: &mut Builder<'_>, name: &str, kind: &str, revision: Option<i64>) -> Result<()> {
    if name.is_empty() || name.len() > 200 || name.chars().any(char::is_control)
        || revision.is_some_and(|n| n <= 0) { return Err(Error::Content); }
    let mut settings = serde_json::json!({"kind": kind});
    if let Some(revision) = revision { settings["legacy_revision"] = revision.into(); }
    builder.push(C::Schedule, vec![field("name", name), field("settings", encoded(&settings)?),
        field("requires_reentry", "true")], None)
}

pub(super) fn sqlite(input: impl Read, wal: Option<&mut dyn Read>, builder: &mut Builder<'_>) -> Result<()> {
    use rusqlite::{Connection, OpenFlags, hooks::{AuthAction, AuthContext, Authorization}};
    let workflow = matches!(builder.request.kind, SourceKind::AuthorityWorkflowSqlite);
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("source.db");
    crate::sqlite::copy_bounded(input, &path)?;
    if let Some(wal) = wal { crate::sqlite::copy_bounded(wal, &temp.path().join("source.db-wal"))?; }
    let mut conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    crate::sqlite::harden(&mut conn)?;
    // The reviewed workflow authority trigger has more than 32 comparisons.
    if workflow { conn.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_EXPR_DEPTH, 128)?; }
    check_sqlite(&conn)?;
    // Autopilot: v1.79.3 (unchanged) or v1.82 (plus the approval-gate ledger,
    // which is never authorized for reading below). Exact match only.
    let catalogs: &[&str] = if workflow {
        &[include_str!("workflow-v1793.sql")]
    } else {
        &[include_str!("autopilot-v1793.sql"), include_str!("autopilot-v182.sql")]
    };
    let actual = crate::sqlite::schema(&conn)?;
    let mut matched = false;
    for catalog in catalogs {
        let expected = Connection::open_in_memory()?;
        expected.execute_batch(catalog)?;
        if actual == crate::sqlite::schema(&expected)? { matched = true; break; }
    }
    if !matched { return Err(Error::Schema); }
    conn.authorizer(Some(|c: AuthContext<'_>| match c.action {
        AuthAction::Select => Authorization::Allow,
        AuthAction::Read { table_name: "autopilot_rules", column_name: "id" | "name" }
        | AuthAction::Read { table_name: "workflow_revisions" | "workflow_candidate_revisions", column_name: "workflow_id" | "revision" }
        | AuthAction::Read { table_name: "workflow_schema_meta", column_name: "owner" | "version" }
            if c.database_name == Some("main") && c.accessor.is_none() => Authorization::Allow,
        _ => Authorization::Deny,
    }))?;
    if workflow {
        let versions: Vec<(String, i64)> = conn.prepare("SELECT owner,version FROM main.workflow_schema_meta ORDER BY owner")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<std::result::Result<_, _>>()?;
        if versions != [("evidence".into(), 1), ("foundation".into(), 1)] { return Err(Error::Schema); }
        // Keep the latest approved and candidate revision as separate decisions;
        // neither old acceptance nor an active run is evidence for fresh approval.
        for (query, kind) in [
            ("SELECT a.workflow_id,a.revision FROM main.workflow_revisions a WHERE NOT EXISTS
                (SELECT 1 FROM main.workflow_revisions b WHERE b.workflow_id=a.workflow_id AND b.revision>a.revision)
                ORDER BY a.workflow_id", "workflow"),
            ("SELECT a.workflow_id,a.revision FROM main.workflow_candidate_revisions a WHERE NOT EXISTS
                (SELECT 1 FROM main.workflow_candidate_revisions b WHERE b.workflow_id=a.workflow_id AND b.revision>a.revision)
                ORDER BY a.workflow_id", "workflow_candidate"),
        ] {
            let mut query = conn.prepare(query)?;
            let mut rows = query.query([])?;
            while let Some(row) = rows.next()? { propose(builder, &row.get::<_, String>(0)?, kind, Some(row.get(1)?))?; }
        }
    } else {
        let mut query = conn.prepare("SELECT name FROM main.autopilot_rules ORDER BY id")?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? { propose(builder, &row.get::<_, String>(0)?, "autopilot", None)?; }
    }
    Ok(())
}

pub(super) fn reminders(input: impl Read, builder: &mut Builder<'_>) -> Result<()> {
    use std::io::{BufRead, BufReader};
    let mut input = BufReader::new(input.take(crate::MAX_SOURCE_BYTES + 1));
    let mut total = 0u64;
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let mut bytes = Vec::new();
        (&mut input).take(64 * 1024 + 1).read_until(b'\n', &mut bytes)?;
        if bytes.is_empty() { break; }
        total = total.checked_add(bytes.len() as u64).ok_or(Error::Limit)?;
        if bytes.len() > 64 * 1024 || total > crate::MAX_SOURCE_BYTES { return Err(Error::Limit); }
        if bytes.iter().all(u8::is_ascii_whitespace) { continue; }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| Error::Content)?;
        let id = member(&value, "id")?;
        if id.is_empty() || id.len() > 128 || !seen.insert(id.to_string()) { return Err(Error::Content); }
        // Default pending matches the old reader. Unknown states are explicitly
        // listed for re-entry, never silently interpreted as completed work.
        let status = value.get("status").map(|v| v.as_str().ok_or(Error::Content)).transpose()?;
        if matches!(status, Some("delivered" | "failed" | "cancelled")) { continue; }
        propose(builder, id, "reminder", None)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(kind: SourceKind) -> ImportRequest {
        ImportRequest { version: 1, migration_id: RequestId([4; 16]), employee: None,
            source_id: "old:scheduling".into(), kind, title: "schedule".into(),
            legacy_agent: None, has_wal: false, blob_handle: None }
    }
    fn db(schema: &str, rows: &str) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        let conn = rusqlite::Connection::open(file.path()).unwrap();
        conn.execute_batch(schema).unwrap(); conn.execute_batch(rows).unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
        drop(conn); file
    }
    fn inert(proposals: &[AuthorityProposal], count: usize) {
        assert_eq!(proposals.len(), count);
        for (i, p) in proposals.iter().enumerate() {
            assert_eq!(p.category, C::Schedule); assert_eq!(p.sequence, i as u64);
            assert!(p.fields.iter().any(|f| f.name == "requires_reentry"));
            assert!(!serde_json::to_string(p).unwrap().contains("SECRET"));
        }
    }
    #[test]
    fn authority_autopilot_proposes_every_rule_without_old_actions_or_history() {
        let file = db(include_str!("autopilot-v1793.sql"), "INSERT INTO autopilot_rules(id,name,enabled,trigger_event,action,created_at)
            VALUES ('a','Enabled',1,'SECRET-EVENT','SECRET-ACTION','now'),('b','Disabled',0,'SECRET-EVENT','SECRET-ACTION','now');
            INSERT INTO autopilot_history VALUES ('h','a','SECRET-NAME','now','SECRET-RESULT','SECRET-DETAILS');");
        let proposals = extract(&request(SourceKind::AuthorityAutopilotSqlite), std::fs::File::open(file.path()).unwrap(), None).unwrap();
        inert(&proposals, 2);
    }
    #[test]
    fn authority_autopilot_v182_is_accepted_and_its_approval_ledger_never_read() {
        let file = db(include_str!("autopilot-v182.sql"), "INSERT INTO autopilot_rules(id,name,enabled,trigger_event,action,created_at)
            VALUES ('a','Gated',1,'SECRET-EVENT','SECRET-ACTION','now');
            INSERT INTO autopilot_approval_dispatch VALUES ('d','SECRET-APPROVAL','a','SECRET-NAME','SECRET-HASH',
                'SECRET-EVENT','SECRET-REF','SECRET-DEDUPE','SECRET-FROZEN','SECRET-DIGEST','settled','SECRET-OUTCOME','SECRET-DETAIL',
                1,'pending',2,'SECRET-NEXT',NULL,'now','now');");
        let proposals = extract(&request(SourceKind::AuthorityAutopilotSqlite), std::fs::File::open(file.path()).unwrap(), None).unwrap();
        inert(&proposals, 1);
    }
    #[test]
    fn authority_workflow_proposes_latest_revisions_without_old_acceptance_or_grants() {
        let file = db(include_str!("workflow-v1793.sql"), "INSERT INTO workflow_revisions VALUES
            ('report',1,'SECRET-HASH','SECRET-GRANTS'),('report',2,'SECRET-HASH','SECRET-GRANTS');
            INSERT INTO workflow_candidate_revisions VALUES ('report',3,'SECRET-HASH','SECRET-GRANTS');");
        let proposals = extract(&request(SourceKind::AuthorityWorkflowSqlite), std::fs::File::open(file.path()).unwrap(), None).unwrap();
        inert(&proposals, 2);
        assert!(proposals[0].fields.iter().any(|f| f.name == "settings" && matches!(&f.value, ProposalValue::Text {value} if value.contains("\"legacy_revision\":2"))));
    }
    #[test]
    fn authority_scheduling_databases_reject_schema_or_version_drift() {
        for (kind, schema) in [(SourceKind::AuthorityAutopilotSqlite, include_str!("autopilot-v1793.sql")),
            (SourceKind::AuthorityAutopilotSqlite, include_str!("autopilot-v182.sql")),
            (SourceKind::AuthorityWorkflowSqlite, include_str!("workflow-v1793.sql"))] {
            for mutation in ["PRAGMA user_version=1;", "CREATE VIEW planted AS SELECT 1;", "CREATE TABLE planted(secret TEXT);"] {
                let file = db(schema, mutation);
                assert!(extract(&request(kind), std::fs::File::open(file.path()).unwrap(), None).is_err());
            }
        }
        let file = db(include_str!("workflow-v1793.sql"), "UPDATE workflow_schema_meta SET version=2;");
        assert!(extract(&request(SourceKind::AuthorityWorkflowSqlite), std::fs::File::open(file.path()).unwrap(), None).is_err());
    }
    #[test]
    fn authority_reminders_propose_pending_records_without_delivery_authority() {
        let raw = b"{\"id\":\"pending\",\"status\":\"pending\",\"chat_id\":\"SECRET-CHAT\",\"on_behalf_of\":\"SECRET-GRANT\"}\n{\"id\":\"default\",\"prompt\":\"SECRET-PROMPT\"}\n{\"id\":\"done\",\"status\":\"delivered\"}\n{\"id\":\"future\",\"status\":\"unknown\",\"new_field\":\"SECRET\"}\n";
        inert(&extract(&request(SourceKind::AuthorityRemindersJsonl), raw.as_slice(), None).unwrap(), 3);
        for raw in [b"{\"id\":\"same\"}\n{\"id\":\"same\"}\n".as_slice(), b"{\"id\":\"bad\",\"status\":1}", b"{\"id\":\"good\"}\ninvalid"] {
            let mut output = Vec::new();
            assert!(emit(&request(SourceKind::AuthorityRemindersJsonl), raw, None, &mut output).is_err());
            assert!(output.is_empty(), "failed source cannot publish its valid prefix");
        }
    }
}
