//! RM-134: `[mcp_keys]` → hashed store migration and expiry notices.
use super::*;
use chrono::Duration;
use duduclaw_auth::mcp_keys::{Expiry, KeyBinding, NewMcpKey};
use duduclaw_core::mcp_key_store::{Authority, KeyState};

const INTERNAL: &str = "ddc_prod_11111111111111111111111111111111";
const LABELED: &str = "ddc_prod_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PLAIN: &str = "ddc_dev_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const LATE: &str = "ddc_prod_cccccccccccccccccccccccccccccccc";

fn write_config(home: &Path, created: DateTime<Utc>) {
    std::fs::write(
        home.join("config.toml"),
        format!(
            "# operator comment survives\n[general]\ndefault_agent = \"anna\"\n\n\
             [mcp_keys.\"{INTERNAL}\"]\nclient_id = \"gateway-internal\"\nis_external = false\n\
             created_at = \"{now}\"\nscopes = [\"admin\"]\n\n\
             [mcp_keys.\"{LABELED}\"]\nclient_id = \"claude-desktop\"\nis_external = true\n\
             created_at = \"{created}\"\nscopes = [\"memory:read\", \"wiki:read\"]\nlabel = \"Claude Desktop\"\n\n\
             [mcp_keys.\"{PLAIN}\"]\nclient_id = \"operator\"\nis_external = false\n\
             created_at = \"{created}\"\nscopes = [\"memory:read\"]\n\n\
             [mcp_keys.\"not-a-key\"]\nclient_id = \"junk\"\ncreated_at = \"{created}\"\n",
            now = Utc::now().to_rfc3339(),
            created = created.to_rfc3339(),
        ),
    )
    .unwrap();
}

fn append(home: &Path, raw: &str, client: &str, scopes: &str) {
    let mut text = config_text(home);
    text.push_str(&format!(
        "\n[mcp_keys.\"{raw}\"]\nclient_id = \"{client}\"\nis_external = false\ncreated_at = \"{}\"\nscopes = [{scopes}]\n",
        Utc::now().to_rfc3339()
    ));
    std::fs::write(home.join("config.toml"), text).unwrap();
}

fn config_text(home: &Path) -> String {
    std::fs::read_to_string(home.join("config.toml")).unwrap()
}

fn audit_lines(home: &Path) -> Vec<Value> {
    std::fs::read_to_string(home.join("security_audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// The audit log is a directory: no write can succeed.
fn break_audit(home: &Path) {
    let path = home.join("security_audit.jsonl");
    let _ = std::fs::remove_file(&path);
    std::fs::create_dir(&path).unwrap();
}

fn fix_audit(home: &Path) {
    std::fs::remove_dir(home.join("security_audit.jsonl")).unwrap();
}

#[test]
fn migration_moves_dashboard_keys_marks_completion_and_is_idempotent() {
    let home = tempfile::tempdir().unwrap();
    let created = Utc::now() - Duration::days(12);
    write_config(home.path(), created);
    let store = McpKeyStore::open(home.path()).unwrap();
    store.record_internal_key(INTERNAL, Utc::now()).unwrap();
    drop(store);

    let report = migrate_config_keys(home.path()).unwrap();
    assert_eq!((report.inserted, report.already_present, report.removed_from_config, report.kept_in_config),
        (2, 0, 2, 0));
    assert!(report.complete && report.ignored.is_empty());

    let text = config_text(home.path());
    assert!(text.starts_with("# operator comment survives"), "format preserved: {text}");
    assert!(text.contains("default_agent = \"anna\""));
    assert!(text.contains(INTERNAL), "gateway-internal key stays in config");
    assert!(text.contains("not-a-key"), "rows that never authenticated are left alone");
    for moved in [LABELED, PLAIN] {
        assert!(!text.contains(moved), "plaintext {moved} left config.toml");
        let key = ks::lookup(home.path(), moved).unwrap().expect("in the store");
        assert_eq!(key.state(Utc::now()), KeyState::Active);
        assert_eq!(ks::timestamp(key.created_at), ks::timestamp(created), "created_at kept");
        // L2: the instant the old `num_days() > 30` rule started refusing it.
        assert_eq!(key.expires_at.unwrap(), ks::legacy_expires_at(created));
        assert!(key.migrated && !key.expiry_reviewed);
    }
    let labeled = ks::lookup(home.path(), LABELED).unwrap().unwrap();
    assert_eq!(labeled.name, "Claude Desktop");
    assert_eq!(labeled.scopes, vec!["memory:read", "wiki:read"]);
    assert!(labeled.is_external);
    assert!(ks::lookup(home.path(), PLAIN).unwrap().unwrap().name.starts_with("migrated-"));
    assert!(matches!(ks::resolve(home.path(), INTERNAL).unwrap(), Authority::Internal(_)));

    // The store holds digests only.
    let bytes = std::fs::read(ks::store_path(home.path())).unwrap();
    for raw in [LABELED, PLAIN] {
        assert!(!bytes.windows(raw.len()).any(|w| w == raw.as_bytes()));
    }
    // One audit record per migrated key, never the key or its digest.
    let audits: Vec<Value> = audit_lines(home.path()).into_iter().filter(|e| e["event_type"] == "mcp_key_migrated").collect();
    assert_eq!(audits.len(), 2);
    let raw_audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    for raw in [LABELED, PLAIN] {
        assert!(!raw_audit.contains(raw));
        assert!(!raw_audit.contains(&ks::digest(raw)));
    }

    // Second start: nothing to do, config untouched.
    let before = config_text(home.path());
    let again = migrate_config_keys(home.path()).unwrap();
    assert_eq!((again.inserted, again.removed_from_config, again.kept_in_config), (0, 0, 0));
    assert!(again.complete);
    assert_eq!(config_text(home.path()), before);
    assert_eq!(audit_lines(home.path()).iter().filter(|e| e["event_type"] == "mcp_key_migrated").count(), 2);
}

/// H1: after the marker a new `[mcp_keys]` entry is never imported, never
/// accepted, and reported once (by hint) — as is a forged gateway-internal
/// entry.
#[test]
fn entries_added_after_the_marker_are_ignored_and_reported() {
    let home = tempfile::tempdir().unwrap();
    write_config(home.path(), Utc::now() - Duration::days(1));
    McpKeyStore::open(home.path()).unwrap().record_internal_key(INTERNAL, Utc::now()).unwrap();
    assert!(migrate_config_keys(home.path()).unwrap().complete);
    let forged = "ddc_prod_dddddddddddddddddddddddddddddddd";
    append(home.path(), LATE, "attacker", "\"admin\"");
    append(home.path(), forged, "gateway-internal", "\"admin\"");
    let report = migrate_config_keys(home.path()).unwrap();
    assert_eq!(report.inserted, 0);
    let mut ignored = report.ignored.clone();
    ignored.sort();
    assert_eq!(ignored, vec![ks::key_hint(LATE), ks::key_hint(forged)]);
    assert!(!ignored.iter().any(|hint| hint.contains(&ks::digest(LATE))), "hints, never fingerprints");
    assert_eq!(ks::resolve(home.path(), LATE).unwrap(), Authority::Unknown);
    assert_eq!(ks::resolve(home.path(), forged).unwrap(), Authority::Unknown);
    assert!(config_text(home.path()).contains(LATE), "left for an Admin to remove");
    // Noted once: a second pass reports them again but they are not new.
    let store = McpKeyStore::open(home.path()).unwrap();
    assert!(!store.note_ignored(&ks::digest(LATE), &ks::key_hint(LATE), Utc::now()).unwrap());
    let listed = legacy_config_report(home.path(), &store).unwrap();
    assert_eq!(listed.pending, 0);
    assert_eq!(listed.ignored.len(), 2);
}

/// H1: the pending set is taken from config as read when the migration
/// began; an entry added while it is still pending (e.g. a failed insert) is
/// not pending.
#[test]
fn only_the_recorded_pending_set_is_imported() {
    let home = tempfile::tempdir().unwrap();
    write_config(home.path(), Utc::now() - Duration::days(1));
    McpKeyStore::open(home.path()).unwrap().record_internal_key(INTERNAL, Utc::now()).unwrap();
    break_audit(home.path());
    let report = migrate_config_keys(home.path()).unwrap();
    assert_eq!((report.inserted, report.kept_in_config, report.removed_from_config), (2, 2, 0));
    assert!(!report.complete, "audit outstanding: still pending");
    // Both are in the store but stay in config until audited (L1).
    let text = config_text(home.path());
    assert!(text.contains(LABELED) && text.contains(PLAIN));
    append(home.path(), LATE, "late", "\"memory:read\"");
    assert_eq!(ks::resolve(home.path(), LATE).unwrap(), Authority::Unknown, "not pending");
    fix_audit(home.path());
    let report = migrate_config_keys(home.path()).unwrap();
    assert_eq!((report.inserted, report.already_present, report.removed_from_config), (0, 2, 2));
    assert_eq!(report.ignored, vec![ks::key_hint(LATE)]);
    assert!(report.complete);
    assert_eq!(audit_lines(home.path()).iter().filter(|e| e["event_type"] == "mcp_key_migrated").count(), 2);
}

#[test]
fn a_tampered_store_schema_refuses_the_migration_and_leaves_config() {
    let home = tempfile::tempdir().unwrap();
    write_config(home.path(), Utc::now() - Duration::days(1));
    drop(McpKeyStore::open(home.path()).unwrap());
    // A trigger makes the store's schema "unexpected": the whole store is
    // refused (fail closed), the config is untouched.
    rusqlite::Connection::open(ks::store_path(home.path()))
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER block_operator BEFORE INSERT ON mcp_api_keys WHEN NEW.client_id = 'operator'
             BEGIN SELECT RAISE(ABORT, 'blocked'); END;",
        )
        .unwrap();
    let before = config_text(home.path());
    assert!(migrate_config_keys(home.path()).is_err());
    assert_eq!(config_text(home.path()), before);
    assert!(ks::resolve(home.path(), PLAIN).is_err(), "an unexpected schema denies");
}

#[test]
fn an_unopenable_store_leaves_config_untouched() {
    let home = tempfile::tempdir().unwrap();
    write_config(home.path(), Utc::now() - Duration::days(1));
    let before = config_text(home.path());
    std::fs::create_dir(ks::store_path(home.path())).unwrap();
    assert!(migrate_config_keys(home.path()).is_err());
    assert_eq!(config_text(home.path()), before);
    // A broken store denies every key (fail closed).
    assert!(ks::lookup(home.path(), LABELED).is_err());
}

#[test]
fn a_deleted_key_is_never_resurrected_from_a_stale_config_copy() {
    let home = tempfile::tempdir().unwrap();
    let created = Utc::now() - Duration::days(1);
    write_config(home.path(), created);
    let original = config_text(home.path());
    migrate_config_keys(home.path()).unwrap();
    let id = ks::lookup(home.path(), LABELED).unwrap().unwrap().id;
    McpKeyStore::open(home.path()).unwrap().delete(&id, "admin", Utc::now()).unwrap();
    // A restored backup brings the plaintext entry back.
    std::fs::write(home.path().join("config.toml"), &original).unwrap();
    let report = migrate_config_keys(home.path()).unwrap();
    assert_eq!(report.inserted, 0);
    assert_eq!(report.already_present, 2);
    assert!(!config_text(home.path()).contains(LABELED));
    assert_eq!(ks::lookup(home.path(), LABELED).unwrap().unwrap().state(Utc::now()), KeyState::Deleted);
}

#[test]
fn a_home_without_legacy_keys_completes_at_once() {
    let home = tempfile::tempdir().unwrap();
    let report = migrate_config_keys(home.path()).unwrap();
    assert!(report.complete && report.inserted == 0);
    assert!(!home.path().join("config.toml").exists(), "never writes config");
    let other = tempfile::tempdir().unwrap();
    std::fs::write(other.path().join("config.toml"), "[general]\n").unwrap();
    assert!(migrate_config_keys(other.path()).unwrap().complete);
    // From now on a key added to config is ignored.
    append(other.path(), LATE, "late", "\"memory:read\"");
    assert_eq!(ks::resolve(other.path(), LATE).unwrap(), Authority::Unknown);
    let broken = tempfile::tempdir().unwrap();
    std::fs::write(broken.path().join("config.toml"), "[broken").unwrap();
    assert!(migrate_config_keys(broken.path()).is_err());
    assert!(!ks::store_path(broken.path()).exists(), "no store, no marker for an unreadable config");
}

/// Third review (High): a legacy internal entry is bound only when its client
/// id is exactly an existing employee directory id — never an `[agent] name`,
/// another case, or an external key.
#[test]
fn migrated_entries_are_bound_only_to_an_exact_employee_directory_id() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents").join("sales");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), "[agent]\nname = \"Sales Team\"\nstatus = \"active\"\n").unwrap();
    std::fs::write(home.path().join("config.toml"), "[general]\n").unwrap();
    let key = |n: u8| format!("ddc_prod_{}{n:02}", "e".repeat(30));
    let (exact, display, cased, gone) = (key(1), key(2), key(3), key(4));
    append(home.path(), &exact, "sales", "\"memory:read\"");
    append(home.path(), &display, "Sales Team", "\"memory:read\"");
    append(home.path(), &cased, "SALES", "\"memory:read\"");
    append(home.path(), &gone, "former-employee", "\"memory:read\"");
    let report = migrate_config_keys(home.path()).unwrap();
    assert_eq!(report.inserted, 4);
    let binding = |raw: &str| ks::lookup(home.path(), raw).unwrap().unwrap().agent_binding;
    assert_eq!(binding(&exact).as_deref(), Some("sales"));
    for unbound in [&display, &cased, &gone] {
        assert_eq!(binding(unbound), None, "{unbound}");
    }
}

/// Third review (Medium): an entry dated beyond the clock-skew allowance never
/// authenticated under the legacy rule; it is not imported (so it cannot gain
/// a far-future expiry) and is reported as ignored.
#[test]
fn a_future_dated_entry_is_never_imported() {
    let home = tempfile::tempdir().unwrap();
    let planted = &format!("ddc_prod_{}01", "f".repeat(30));
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "[mcp_keys.\"{planted}\"]\nclient_id = \"planted\"\nis_external = false\n\
             created_at = \"{}\"\nscopes = [\"memory:read\"]\n",
            (Utc::now() + Duration::days(400)).to_rfc3339()
        ),
    )
    .unwrap();
    let report = migrate_config_keys(home.path()).unwrap();
    assert_eq!(report.inserted, 0);
    assert_eq!(report.ignored, vec![ks::key_hint(planted)]);
    assert!(report.complete);
    assert_eq!(ks::resolve(home.path(), planted).unwrap(), Authority::Unknown);
    let store = McpKeyStore::open(home.path()).unwrap();
    assert_eq!(legacy_config_report(home.path(), &store).unwrap().ignored, vec![ks::key_hint(planted)]);
}

/// Fourth review (Medium 2): an employee's legacy entry is bound only when
/// every scope is a per-employee scope. With `admin` (or any other scope) it
/// is imported unbound — scopes kept, nothing downgraded — and flagged for
/// re-issue.
#[test]
fn a_migrated_employee_entry_with_non_employee_scopes_is_imported_unbound_and_flagged() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents").join("alice");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), "[agent]\nname = \"Alice\"\n").unwrap();
    std::fs::write(home.path().join("config.toml"), "[general]\n").unwrap();
    let key = |n: u8| format!("ddc_prod_{}{n:02}", "a".repeat(30));
    let (admin, mixed, fine) = (key(1), key(2), key(3));
    append(home.path(), &admin, "alice", "\"admin\"");
    append(home.path(), &mixed, "alice", "\"memory:read\", \"team:handoff\"");
    append(home.path(), &fine, "alice", "\"memory:read\", \"google:read\"");
    let report = migrate_config_keys(home.path()).unwrap();
    assert_eq!(report.inserted, 3);
    let row = |raw: &str| ks::lookup(home.path(), raw).unwrap().unwrap();
    assert_eq!((row(&admin).agent_binding, row(&admin).scopes), (None, vec!["admin".to_string()]), "kept verbatim, unbound");
    assert_eq!(row(&mixed).agent_binding, None);
    assert_eq!(row(&mixed).scopes, vec!["memory:read", "team:handoff"]);
    assert_eq!(row(&fine).agent_binding.as_deref(), Some("alice"));
    // The keys-page flag.
    assert_eq!(needs_reissue(&row(&admin), true), Some(NEEDS_REISSUE_EMPLOYEE_SCOPES));
    assert_eq!(needs_reissue(&row(&mixed), true), Some(NEEDS_REISSUE_EMPLOYEE_SCOPES));
    assert_eq!(needs_reissue(&row(&fine), true), None, "bound: fine");
    assert_eq!(needs_reissue(&row(&admin), false), None, "not an employee's entry");
    // The audit record of the unbound import names no binding.
    let migrated = audit_lines(home.path());
    assert!(migrated.iter().filter(|e| e["event_type"] == "mcp_key_migrated").any(|e| e["details"]["agent_binding"].is_null()
        && e["details"]["scopes"] == json!(["admin"])));
}

fn employee_dir(home: &Path, id: &str, status: &str) {
    let dir = home.join("agents").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), format!("[agent]\nname = \"{id}\"\nstatus = \"{status}\"\n")).unwrap();
}

fn employee_key(home: &Path, name: &str, agent: &str, now: DateTime<Utc>) -> (String, StoredKey) {
    let created = McpKeyStore::open(home)
        .unwrap()
        .create(
            NewMcpKey {
                name: name.into(),
                client_id: agent.into(),
                binding: KeyBinding::Employee,
                env: "prod".into(),
                scopes: vec!["memory:read".into()],
                expiry: Expiry::Never,
                created_by: Some("admin".into()),
            },
            now,
        )
        .unwrap();
    (created.secret, created.key)
}

/// Fourth review (Medium 1): an employee removed OUTSIDE the gateway (MCP
/// `agent_remove` moves the directory to `agents/_trash`) or soft-deleted
/// has its keys tombstoned, audited, by the lock holder's sweep — before the
/// id can be re-created. A later employee with the same id never gets them
/// back; a new key for it works.
#[test]
fn the_key_sweep_tombstones_keys_of_employees_removed_outside_the_gateway() {
    let home = tempfile::tempdir().unwrap();
    let now = Utc::now();
    for id in ["alice", "bob", "carol", "dave"] {
        employee_dir(home.path(), id, "active");
    }
    let (alice_secret, alice) = employee_key(home.path(), "alice key", "alice", now);
    let (bob_secret, _) = employee_key(home.path(), "bob key", "bob", now);
    let (carol_secret, _) = employee_key(home.path(), "carol key", "carol", now);
    let (dave_secret, _) = employee_key(home.path(), "dave key", "dave", now);
    let (zapier_secret, _) = issue(home.path(), "zapier", Expiry::Never);
    assert_eq!(sweep_employee_gone_keys(home.path(), std::time::Duration::ZERO).unwrap(), 0, "everyone is live");

    // MCP agent_remove: the directory moves to the trash. Bob is soft-deleted.
    // Dave's agent.toml is unreadable (left alone: may be transient).
    std::fs::create_dir_all(home.path().join("agents/_trash")).unwrap();
    std::fs::rename(home.path().join("agents/alice"), home.path().join("agents/_trash/alice_20261113000000")).unwrap();
    employee_dir(home.path(), "bob", "deleted");
    std::fs::write(home.path().join("agents/dave/agent.toml"), "[agent\nbroken").unwrap();
    assert_eq!(sweep_employee_gone_keys(home.path(), std::time::Duration::ZERO).unwrap(), 2);
    let state = |secret: &str| ks::lookup(home.path(), secret).unwrap().unwrap().state(Utc::now());
    assert_eq!(state(&alice_secret), KeyState::Deleted);
    assert_eq!(state(&bob_secret), KeyState::Deleted);
    for live in [&carol_secret, &dave_secret, &zapier_secret] {
        assert_eq!(state(live), KeyState::Active);
    }
    let revoked: Vec<Value> = audit_lines(home.path()).into_iter().filter(|e| e["event_type"] == "mcp_key_deleted").collect();
    assert_eq!(revoked.len(), 2);
    let mut reasons: Vec<&str> = revoked.iter().map(|e| e["details"]["reason"].as_str().unwrap()).collect();
    reasons.sort();
    assert_eq!(reasons, vec!["employee_deleted", "employee_missing"]);
    assert!(revoked.iter().all(|e| e["details"]["source"] == "key_sweep" && e["details"]["permanent"] == true
        && e["details"]["actor"] == duduclaw_auth::mcp_keys::EMPLOYEE_GONE_ACTOR));
    let raw = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert!(!raw.contains(&alice_secret) && !raw.contains(&ks::digest(&alice_secret)), "never the key or its digest");
    let tombstone = McpKeyStore::open(home.path()).unwrap().get_any(&alice.id).unwrap();
    assert!(tombstone.deleted_at.is_some());
    assert_eq!(tombstone.agent_binding.as_deref(), Some("alice"), "kept on the tombstone");

    // A different employee is created later under the same id.
    employee_dir(home.path(), "alice", "active");
    assert_eq!(state(&alice_secret), KeyState::Deleted, "the tombstone never comes back");
    let (fresh_secret, fresh) = employee_key(home.path(), "new alice key", "alice", Utc::now());
    assert_eq!(state(&fresh_secret), KeyState::Active);
    assert_eq!(ks::key_binding_state(home.path(), "alice", fresh.created_at), ks::BindingState::Live);
    assert_eq!(sweep_employee_gone_keys(home.path(), std::time::Duration::ZERO).unwrap(), 0, "the new key is kept");
    assert_eq!(state(&fresh_secret), KeyState::Active);

    // A home whose whole `agents/` directory is missing decides nothing.
    std::fs::rename(home.path().join("agents"), home.path().join("agents.moved")).unwrap();
    assert_eq!(sweep_employee_gone_keys(home.path(), std::time::Duration::ZERO).unwrap(), 0);
    assert_eq!(state(&carol_secret), KeyState::Active);
}

/// Fourth review (Medium 1): the residual window — an employee removed
/// outside the gateway and re-created under the same id BEFORE the next
/// sweep. Where the platform reports a directory birth time the old key is
/// refused (it predates the employee); it is not tombstoned on that
/// evidence. The new employee's own key works.
#[test]
fn a_key_older_than_a_recreated_employee_is_refused_before_any_sweep() {
    let home = tempfile::tempdir().unwrap();
    let issued = Utc::now() - Duration::days(60);
    employee_dir(home.path(), "alice", "active");
    let (_, old) = employee_key(home.path(), "old alice key", "alice", issued);
    // Removed and re-created with no sweep in between.
    std::fs::remove_dir_all(home.path().join("agents/alice")).unwrap();
    employee_dir(home.path(), "alice", "active");
    let birth_known = ks::employee_dir_birth(home.path(), "alice").is_some();
    let expected = if birth_known { ks::BindingState::Recreated } else { ks::BindingState::Live };
    assert_eq!(ks::key_binding_state(home.path(), "alice", old.created_at), expected);
    assert_eq!(sweep_employee_gone_keys(home.path(), std::time::Duration::ZERO).unwrap(), 0, "refused, not destroyed");
    let (_, fresh) = employee_key(home.path(), "new alice key", "alice", Utc::now());
    assert_eq!(ks::key_binding_state(home.path(), "alice", fresh.created_at), ks::BindingState::Live);
}

fn issue(home: &Path, name: &str, expiry: Expiry) -> (String, StoredKey) {
    let created = McpKeyStore::open(home)
        .unwrap()
        .create(
            NewMcpKey {
                name: name.into(),
                client_id: "zapier".into(),
                binding: KeyBinding::Integration,
                env: "prod".into(),
                scopes: vec!["memory:read".into()],
                expiry,
                created_by: Some("admin".into()),
            },
            Utc::now(),
        )
        .unwrap();
    (created.secret, created.key)
}

#[tokio::test]
async fn expiry_notices_are_delivered_once_per_key_per_threshold() {
    let home = tempfile::tempdir().unwrap();
    let now = Utc::now();
    let (secret, key) = issue(home.path(), "Zapier 整合", Expiry::At(now + Duration::days(6)));
    issue(home.path(), "forever", Expiry::Never);
    let ok = |_: StoredKey, _: NoticeThreshold| async { true };
    assert_eq!(sweep_expiry_notices_with(home.path(), now, ok).await, 1, "7-day notice");
    assert_eq!(sweep_expiry_notices_with(home.path(), now, ok).await, 0, "only once");
    assert_eq!(sweep_expiry_notices_with(home.path(), now + Duration::days(2), ok).await, 0);
    let one_day = key.expires_at.unwrap() - Duration::hours(20);
    assert_eq!(sweep_expiry_notices_with(home.path(), one_day, ok).await, 1, "1-day notice");
    assert_eq!(sweep_expiry_notices_with(home.path(), one_day, ok).await, 0, "only once");
    assert_eq!(sweep_expiry_notices_with(home.path(), key.expires_at.unwrap() + Duration::hours(1), ok).await, 0);
    let notices: Vec<Value> = audit_lines(home.path()).into_iter().filter(|e| e["event_type"] == "mcp_key_expiry_notice").collect();
    assert_eq!(notices.len(), 2);
    assert_eq!(notices[0]["details"]["threshold"], "7d");
    assert_eq!(notices[1]["details"]["threshold"], "1d");
    assert_eq!(notices[0]["details"]["key_id"], key.id.as_str());
    let raw = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert!(!raw.contains(&secret) && !raw.contains(&ks::digest(&secret)));
    // The persisted state survives a "restart" (a fresh store handle).
    let store = McpKeyStore::open(home.path()).unwrap();
    assert_eq!(store.notice_state(&key, NoticeThreshold::SevenDays).unwrap().unwrap().0, "delivered");
    assert_eq!(store.notice_state(&key, NoticeThreshold::OneDay).unwrap().unwrap().0, "delivered");
}

/// M2: a failed send is not marked sent; it is retried after the backoff and
/// delivered exactly once.
#[tokio::test]
async fn a_failed_notice_is_retried_and_delivered_once() {
    let home = tempfile::tempdir().unwrap();
    let now = Utc::now();
    let (_, key) = issue(home.path(), "flaky", Expiry::At(now + Duration::days(6)));
    let mut attempts = 0;
    let mut flaky = |_: StoredKey, _: NoticeThreshold| {
        attempts += 1;
        let ok = attempts > 1;
        async move { ok }
    };
    assert_eq!(sweep_expiry_notices_with(home.path(), now, &mut flaky).await, 0, "first attempt fails");
    let store = McpKeyStore::open(home.path()).unwrap();
    assert_eq!(store.notice_state(&key, NoticeThreshold::SevenDays).unwrap(), Some(("pending".into(), 1)));
    assert!(audit_lines(home.path()).iter().all(|e| e["event_type"] != "mcp_key_expiry_notice"), "not recorded as sent");
    assert_eq!(sweep_expiry_notices_with(home.path(), now + Duration::seconds(30), &mut flaky).await, 0, "backoff");
    let retry = now + Duration::seconds(duduclaw_auth::mcp_keys::NOTICE_RETRY_BASE_SECS + 1);
    assert_eq!(sweep_expiry_notices_with(home.path(), retry, &mut flaky).await, 1, "retried and delivered");
    assert_eq!(sweep_expiry_notices_with(home.path(), retry + Duration::hours(1), &mut flaky).await, 0, "once");
    assert_eq!(attempts, 2);
    assert_eq!(store.notice_state(&key, NoticeThreshold::SevenDays).unwrap(), Some(("delivered".into(), 2)));
}

/// Second review (Low): a notice claimed by a sweep whose key an Admin
/// changes before it is sent (here: while the sweep is sending the other
/// key's notice) is re-checked by CAS and dropped — never sent with the old
/// date — and the expiry change closed it in its own transaction.
#[tokio::test]
async fn a_notice_whose_key_changed_after_the_claim_is_dropped_not_sent() {
    let home = tempfile::tempdir().unwrap();
    let now = Utc::now();
    let (_, a) = issue(home.path(), "first", Expiry::At(now + Duration::days(6)));
    let (_, b) = issue(home.path(), "second", Expiry::At(now + Duration::days(6)));
    let target = home.path().to_path_buf();
    let ids = [a.id.clone(), b.id.clone()];
    let mut sent: Vec<String> = Vec::new();
    let delivered = sweep_expiry_notices_with(home.path(), now, |key: StoredKey, _: NoticeThreshold| {
        if sent.is_empty() {
            // Both notices are claimed by now; change the other key.
            let other = ids.iter().find(|id| **id != key.id).unwrap();
            McpKeyStore::open(&target).unwrap().set_expiry(other, Expiry::Days(90), now).unwrap();
        }
        sent.push(key.id.clone());
        async { true }
    })
    .await;
    assert_eq!(delivered, 1);
    assert_eq!(sent.len(), 1, "the changed key's stale notice is never sent");
    let stale = if sent[0] == a.id { &b } else { &a };
    let store = McpKeyStore::open(home.path()).unwrap();
    assert_eq!(store.notice_state(stale, NoticeThreshold::SevenDays).unwrap().unwrap().0, "closed");
    let notices = audit_lines(home.path()).into_iter().filter(|e| e["event_type"] == "mcp_key_expiry_notice").count();
    assert_eq!(notices, 1);
    // Its new date is 90 days out: nothing is owed for it now.
    let ok = |_: StoredKey, _: NoticeThreshold| async { true };
    assert_eq!(sweep_expiry_notices_with(home.path(), now, ok).await, 0);
}

/// Third review (Low): the accepted window — the key changes after the final
/// confirm, while the transport is sending. The finalise CAS matches nothing,
/// so the sweep neither counts nor audits a delivery.
#[tokio::test]
async fn a_notice_whose_key_changes_during_the_send_is_not_counted_or_audited() {
    let home = tempfile::tempdir().unwrap();
    let now = Utc::now();
    let (_, key) = issue(home.path(), "racy", Expiry::At(now + Duration::days(6)));
    let target = home.path().to_path_buf();
    let delivered = sweep_expiry_notices_with(home.path(), now, |sent: StoredKey, _: NoticeThreshold| {
        McpKeyStore::open(&target).unwrap().rename(&sent.id, "renamed mid-send").unwrap();
        async { true }
    })
    .await;
    assert_eq!(delivered, 0, "not counted");
    assert!(audit_lines(home.path()).iter().all(|e| e["event_type"] != "mcp_key_expiry_notice"), "not audited");
    let store = McpKeyStore::open(home.path()).unwrap();
    assert_eq!(store.notice_state(&key, NoticeThreshold::SevenDays).unwrap().unwrap().0, "pending");
}

/// Fourth review (Low): a send that fails after the key changed version
/// (here: renamed during the send) is stale — the failure CAS matches
/// nothing, so the sweep reschedules nothing and records no failure; the
/// notice stays owed and, once this claim's lease is over, is owed for the
/// current row with no backoff in the way (the store-level test
/// `notice_failed_is_a_cas_bound_to_the_live_claim_and_key_version` checks
/// the columns).
#[tokio::test]
async fn a_failed_send_for_a_key_that_changed_meanwhile_is_not_rescheduled() {
    let home = tempfile::tempdir().unwrap();
    let now = Utc::now();
    let (_, key) = issue(home.path(), "changing", Expiry::At(now + Duration::days(6)));
    let target = home.path().to_path_buf();
    let delivered = sweep_expiry_notices_with(home.path(), now, |sent: StoredKey, _: NoticeThreshold| {
        McpKeyStore::open(&target).unwrap().rename(&sent.id, "renamed mid-send").unwrap();
        async { false }
    })
    .await;
    assert_eq!(delivered, 0);
    assert!(audit_lines(home.path()).iter().all(|e| e["event_type"] != "mcp_key_expiry_notice"));
    let store = McpKeyStore::open(home.path()).unwrap();
    assert_eq!(store.notice_state(&key, NoticeThreshold::SevenDays).unwrap(), Some(("pending".into(), 1)));
    let after_lease = now + Duration::seconds(duduclaw_auth::mcp_keys::NOTICE_CLAIM_LEASE_SECS + 1);
    let current = store.get(&key.id).unwrap();
    assert!(store.claim_notice(&current, NoticeThreshold::SevenDays, after_lease).unwrap().is_some(),
        "no retry backoff was scheduled by the stale failure");
}

#[tokio::test]
async fn disabled_keys_and_homes_without_a_store_get_no_notice() {
    let home = tempfile::tempdir().unwrap();
    let ok = |_: StoredKey, _: NoticeThreshold| async { true };
    assert_eq!(sweep_expiry_notices_with(home.path(), Utc::now(), ok).await, 0);
    assert!(!ks::store_path(home.path()).exists());
    let now = Utc::now();
    let (_, key) = issue(home.path(), "off", Expiry::At(now + Duration::days(3)));
    McpKeyStore::open(home.path()).unwrap().set_disabled(&key.id, true, "admin", now).unwrap();
    assert_eq!(sweep_expiry_notices_with(home.path(), now, ok).await, 0);
    // Without any Admin destination the real path reports "not delivered".
    let (_, live) = issue(home.path(), "live", Expiry::At(now + Duration::days(3)));
    assert_eq!(sweep_expiry_notices(home.path(), now).await, 0);
    let store = McpKeyStore::open(home.path()).unwrap();
    assert_eq!(store.notice_state(&live, NoticeThreshold::SevenDays).unwrap().unwrap().0, "pending");
}

#[test]
fn notice_text_names_the_key_and_date_without_secrets() {
    let home = tempfile::tempdir().unwrap();
    let (secret, key) = issue(home.path(), "CI", Expiry::Days(30));
    for threshold in [NoticeThreshold::SevenDays, NoticeThreshold::OneDay] {
        let text = notice_text(&key, threshold);
        assert!(text.contains("「CI」") && text.contains(&ks::timestamp(key.expires_at.unwrap())));
        assert!(!text.contains(&secret) && !text.contains(&key.hint));
    }
}
