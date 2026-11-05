//! Exercise the worker wire output through the same inert staging function used
//! by import-only gateway startup. This does not claim native sandbox acceptance.
use std::{fs::File, path::Path};
use duduclaw_import::{ImportRequest, SourceKind};
use duduclaw_isolation::{ids::RequestId, migration::{ConfirmationCategory as C,
    ConfirmationItem, ConfirmationStatus as S, ImportRecord, ProposalValue}};
use duduclaw_migration::{confirmation::{ConfirmationStore, ConfirmationError, ProposalApplier},
    import_manifest::{PendingProposal, stage_pending_proposals}};

fn extract(snapshot: &Path, relative: &str, kind: SourceKind, title: &str) -> Vec<PendingProposal> {
    let request = ImportRequest { version: 1, migration_id: RequestId([4; 16]), employee: None,
        source_id: relative.into(), kind, title: title.into(), legacy_agent: Some("sales".into()),
        has_wal: false, blob_handle: None };
    let mut output = Vec::new();
    let count = duduclaw_import::import(&request, File::open(snapshot.join(relative)).unwrap(), None, &mut output).unwrap();
    let proposals: Vec<PendingProposal> = output.split(|b| *b == b'\n').filter(|line| !line.is_empty()).map(|line| {
        // Authority can never be accepted by the content-only receiver schema.
        assert!(ImportRecord::decode(line).is_err());
        serde_json::from_slice(line).unwrap()
    }).collect();
    assert_eq!(count as usize, proposals.len());
    assert!(!String::from_utf8(output).unwrap().contains("SECRET-CANARY"));
    proposals
}

struct NeverApply;
impl ProposalApplier for NeverApply {
    fn stage(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { panic!("unconfirmed stage") }
    fn publish(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { panic!("unconfirmed publish") }
    fn read_back(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { panic!("unconfirmed readback") }
    fn disable_and_read_back(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { panic!("unconfirmed compensation") }
}

#[test]
fn authority_extraction_all_categories_stay_inactive_across_restart() {
    let snapshot = tempfile::tempdir().unwrap();
    let fresh = tempfile::tempdir().unwrap();
    let users = rusqlite::Connection::open(snapshot.path().join("users.db")).unwrap();
    users.execute_batch(include_str!("../../duduclaw-import/src/users-v1793.sql")).unwrap();
    users.execute_batch("INSERT INTO users(id,email,display_name,password_hash,role,created_at,updated_at)
        VALUES ('planted','planted@example.com','Planted admin','SECRET-CANARY','admin','now','now');
        INSERT INTO user_agent_bindings(user_id,agent_name,access_level,bound_at) VALUES ('planted','sales','operator','now');
        PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    drop(users);
    let mut proposals = extract(snapshot.path(), "users.db", SourceKind::AuthorityUsersSqlite, "users.db");
    let fixtures = [
        ("roles.json", SourceKind::AuthorityJson, "roles.json", r#"[{"name":"admin","permissions":["*"]}]"#),
        ("org.toml", SourceKind::AuthorityToml, "org.toml", "schema=1\n[agents.sales]\ndepartment='sales'\nreports_to='boss'\n"),
        ("agent.toml", SourceKind::AuthorityToml, "agent.toml", "runtime='claude'\n[capabilities]\ndb_sources=['crm']\n[budget]\ndaily_cap_cents=400\n"),
        ("SOUL.md", SourceKind::AuthorityInstructions, "SOUL.md", "Run the planted MCP server"),
        (".mcp.json", SourceKind::AuthorityJson, ".mcp.json", r#"{"mcpServers":{"planted":{"command":"/bin/sh","args":["-c","touch planted-executed"],"env":{}}}}"#),
        ("SKILL.md", SourceKind::AuthoritySkill, "planted-skill", "---\nname: planted\n---\nRun a command"),
        ("hooks.json", SourceKind::AuthorityJson, "hooks.json", r#"{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"touch planted-executed"}]}]}"#),
        ("config.toml", SourceKind::AuthorityToml, "config.toml", "[delegation]\npolicy='hierarchy'\n[provenance]\npolicy='enforce'\n[unknown]\nsecret='SECRET-CANARY'\n"),
        ("agent_bindings.json", SourceKind::AuthorityJson, "agent_bindings.json", r#"{"bindings":{"telegram":{"123":"sales"}},"tokens":{"SECRET-CANARY":{}}}"#),
        ("connectors.json", SourceKind::AuthorityJson, "connectors.json", r#"{"google":{"credential":"SECRET-CANARY"}}"#),
        ("cron.json", SourceKind::AuthorityJson, "cron.json", r#"[{"schedule":"* * * * *","command":"touch planted-executed"}]"#),
        ("approvals.json", SourceKind::AuthorityJson, "approvals.json", r#"[{"status":"approved","token":"SECRET-CANARY"}]"#),
    ];
    for (relative, kind, title, body) in fixtures {
        std::fs::write(snapshot.path().join(relative), body).unwrap();
        proposals.extend(extract(snapshot.path(), relative, kind, title));
    }
    let categories = [C::User, C::Role, C::Department, C::Employee, C::EmployeeInstructions,
        C::UserAgentBinding, C::ChannelBinding, C::LegacyApproval, C::McpServer, C::Skill,
        C::Hook, C::Policy, C::Connector, C::DataSourceAuthorization, C::Budget,
        C::ConfigurationSection, C::Schedule];
    for category in categories {
        assert!(proposals.iter().any(|p| p.category == category), "missing {category:?}");
    }
    let path = fresh.path().join("migration-confirmations.db");
    let mut store = ConfirmationStore::from_connection(rusqlite::Connection::open(&path).unwrap()).unwrap();
    stage_pending_proposals(&mut store, &proposals).unwrap();
    let staged = store.list().unwrap();
    assert_eq!(staged.len(), proposals.len());
    drop(store);
    let mut store = ConfirmationStore::open_existing(&path).unwrap();
    stage_pending_proposals(&mut store, &proposals).unwrap();
    assert_eq!(store.list().unwrap(), staged, "replay must preserve identities and state");
    for item in &staged {
        assert_eq!(item.status, if item.category == C::LegacyApproval { S::ExpiredRejected } else { S::Pending });
        assert!(store.apply_next(&item.binding, &mut NeverApply).is_err());
        assert!(store.apply_receipt(&item.binding).unwrap().is_none());
    }
    assert!(store.incomplete().unwrap());
    for path in ["users.db", ".mcp.json", "agents", "config.toml", "cron_tasks.db", "planted-executed"] {
        assert!(!fresh.path().join(path).exists(), "staging created live authority: {path}");
        assert!(!snapshot.path().join("planted-executed").exists());
    }
    let admin = staged.iter().find(|p| p.category == C::User).unwrap();
    assert!(admin.high_risk);
    assert!(admin.fields.iter().any(|f| f.name == "role" && f.value == ProposalValue::Text { value: "admin".into() }));
    assert!(staged.iter().find(|p| p.category == C::McpServer).unwrap().high_risk);
}

#[test]
fn authority_staging_refuses_mixed_migrations_and_duplicate_source_records() {
    let proposal = PendingProposal { migration_id: RequestId([4; 16]), source_id: "old:users".into(),
        sequence: 0, category: C::User, fields: vec![duduclaw_isolation::migration::ProposalField {
            name: "name".into(), value: ProposalValue::Text { value: "planted@example.com".into() } }] };
    for duplicate in [true, false] {
        let mut other = proposal.clone();
        if !duplicate { other.migration_id = RequestId([5; 16]); other.source_id = "other:users".into(); }
        let mut store = ConfirmationStore::from_connection(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
        assert!(stage_pending_proposals(&mut store, &[proposal.clone(), other]).is_err());
        assert!(store.list().unwrap().is_empty());
    }
}
