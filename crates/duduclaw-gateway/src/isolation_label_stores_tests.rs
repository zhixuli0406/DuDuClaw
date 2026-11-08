//! F6h (design r5 §3.3, §3.10 step 7, §3.12): the label-keyed stores
//! registry, the writer gate on every writer path, the global route pause
//! and the R7 proposal refusal, against the real stores in a private home.
//! The lifecycle-dependent tests (bot gate, tombstone) are in
//! `isolation_label_stores_lifecycle_tests.rs` (a child of the lifecycle
//! test module, which owns the confirmed-employee fixtures).
use super::*;
use crate::isolation_quarantine as quarantine;
use std::collections::BTreeSet;

const LABEL: &str = "alice-desk";
const OTHER: &str = "bob-desk";
const NOW: &str = "2026-11-08T00:00:00Z";

/// Protected mode forced on (or off) for this thread while the guard lives.
struct Mode;
impl Mode {
    fn protected(on: bool) -> Self { TEST_PROTECTED.with(|cell| cell.set(Some(on))); Mode }
}
impl Drop for Mode {
    fn drop(&mut self) { TEST_PROTECTED.with(|cell| cell.set(None)); }
}

fn private_home() -> tempfile::TempDir { crate::fs_safe::private_test_home().unwrap() }

fn private_dir(path: &Path) {
    #[cfg(unix)]
    { use std::os::unix::fs::DirBuilderExt; std::fs::DirBuilder::new().mode(0o700).create(path).unwrap(); }
    #[cfg(not(unix))]
    std::fs::create_dir(path).unwrap();
}

fn apply_db(home: &Path) -> Connection {
    let dir = home.join(APPLY_DIR);
    if !dir.exists() { private_dir(&dir); }
    let db = Connection::open(dir.join(APPLY_DB)).unwrap();
    quarantine::ensure_employee_schema(&db).unwrap();
    db
}

fn quarantine_row(home: &Path, label: &str, employee: u8, seal: &'static str) {
    let db = apply_db(home);
    assert!(quarantine::insert_quarantine(&db, &quarantine::NewQuarantine {
        employee: EmployeeId([employee; 16]), incarnation: 0, label, owner: None, tenant: "tenant-a",
        quarantined_at: 1, root: None, seal, principal_snapshot: &json!({"kind": "shared"}), stripped: &json!({}),
    }).unwrap());
}

fn set_seal(home: &Path, label: &str, seal: &str) {
    apply_db(home).execute("UPDATE employee_quarantine SET seal=?1 WHERE label=?2", params![seal, label]).unwrap();
}

/// Create every SQLite store through its real constructor (schemas only).
fn real_stores(home: &Path) {
    duduclaw_memory::SqliteMemoryEngine::new(&home.join("memory.db")).unwrap();
    crate::session::SessionManager::new(&home.join("sessions.db")).unwrap();
    crate::task_store::TaskStore::open(home).unwrap();
    crate::cron_store::CronStore::open(home).unwrap();
}

fn memory_entry(label: &str) -> duduclaw_core::types::MemoryEntry {
    duduclaw_core::types::MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(), agent_id: label.to_owned(), content: format!("{label} remembers"),
        timestamp: chrono::Utc::now(), tags: vec![], embedding: None,
        layer: duduclaw_core::types::MemoryLayer::Episodic, importance: 5.0, access_count: 0,
        last_accessed: None, source_event: "f6h_test".to_owned(),
    }
}

fn task_row(id: &str, label: &str) -> crate::task_store::TaskRow {
    crate::task_store::TaskRow::new(id.to_owned(), "report".into(), String::new(), "medium".into(), label.into(), "system".into())
}

fn cron_row(id: &str, label: &str) -> crate::cron_store::CronTaskRow {
    crate::cron_store::CronTaskRow::new(id.to_owned(), format!("{id}-name"), label.into(), "0 * * * *".into(), "report".into())
}

/// One row under `label` in every SQLite store, through the real writers.
async fn seed(home: &Path, label: &str) {
    use duduclaw_core::traits::MemoryEngine;
    let memory = duduclaw_memory::SqliteMemoryEngine::new(&home.join("memory.db")).unwrap();
    memory.store(label, memory_entry(label)).await.unwrap();
    let sessions = crate::session::SessionManager::new(&home.join("sessions.db")).unwrap();
    let session = format!("webchat:{label}");
    sessions.get_or_create(&session, label).await.unwrap();
    sessions.append_message(&session, "user", "hello", 1).await.unwrap();
    crate::task_store::TaskStore::open(home).unwrap().insert_task(&task_row(&format!("task-{label}"), label)).await.unwrap();
    crate::cron_store::CronStore::open(home).unwrap().insert(&cron_row(&format!("cron-{label}"), label)).await.unwrap();
}

/// Config-map entries: `label` in org.toml (reporting to nobody), `other`
/// reporting to `label`, a channel binding and a bind token to `label`, a
/// website credential shared with `other` and one granted to `label` only.
fn seed_config(home: &Path, label: &str, other: &str) {
    use duduclaw_core::org_store::{self as org, OrgEntry};
    org::with_org_lock(home, |guard| {
        org::upsert(guard, label, OrgEntry::new("", "sales"))?;
        org::upsert(guard, other, OrgEntry::new(label, "sales"))?;
        Ok(())
    }).unwrap();
    let bindings = json!({
        "bindings": {"telegram": {"u1": label, "u2": other}},
        "tokens": {"d1": {"token_sha256": "d1", "channel": "telegram", "agent_id": label,
            "expires_at": "2099-01-01T00:00:00Z", "max_uses": 1, "used": 0}},
    });
    let path = home.join("agent_bindings.json");
    std::fs::write(&path, bindings.to_string()).unwrap();
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap(); }
    for (name, agents) in [("crm", vec![label.to_owned(), other.to_owned()]), ("erp", vec![label.to_owned()])] {
        crate::website_credentials::upsert(home, crate::website_credentials::Upsert {
            id: None, label: name.into(), login_url: format!("https://{name}.example.test/login"), allowed_paths: vec![],
            username: "svc".into(), password: "secret-password".into(), allowed_agents: agents,
        }, "admin-1").unwrap();
    }
}

/// users.db member bindings: one user bound to `label`, one to `other`.
fn seed_bindings(home: &Path, label: &str, other: &str) {
    let path = home.join("users.db");
    let users = duduclaw_auth::UserDb::new(&path).unwrap();
    for (n, agent) in [(1, label), (2, other)] {
        let user = users.create_user(&format!("member{n}-{agent}@example.test"), "Member", "password1",
            duduclaw_auth::UserRole::Employee).unwrap();
        users.bind_agent(&user.id, agent, duduclaw_auth::AccessLevel::Operator).unwrap();
    }
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap(); }
}

fn store(name: &str) -> &'static LabelStore {
    LABEL_KEYED_STORES.iter().find(|store| store.name == name).unwrap()
}

fn sqlite_stores() -> impl Iterator<Item = &'static LabelStore> {
    LABEL_KEYED_STORES.iter().filter(|store| matches!(store.backend, Backend::Sqlite { .. }))
}

/// Home databases the real constructors create that hold no agent label.
const NOT_LABEL_KEYED_DBS: [&str; 0] = [];
/// Config maps keyed by agent label (§3.3, S9) and where they live.
const KNOWN_AGENT_KEYED_CONFIG: [&str; 3] = ["org.toml", "secrets/website_credentials.enc", "agent_bindings.json"];
/// Columns whose name mentions an agent but hold no label.
const NOT_A_LABEL: [&str; 1] = ["agent_seconds"];

/// S9: one registry; every §3.3 row and every agent-keyed config map is an
/// entry, every database the stores create is registered, and a schema scan
/// finds no agent/label column the store operations would miss.
#[test]
fn label_keyed_stores_registry_complete() {
    let dir = private_home();
    let home = dir.path();
    real_stores(home);
    let names: BTreeSet<&str> = LABEL_KEYED_STORES.iter().map(|store| store.name).collect();
    assert_eq!(names.len(), LABEL_KEYED_STORES.len(), "store names are unique");
    for required in ["org_entry", "memory", "sessions", "tasks", "cron", "website_credentials", "channel_routes",
        "delegation_targets", "member_bindings"] {
        assert!(names.contains(required), "{required} is registered");
    }
    let sources: BTreeSet<&str> = LABEL_KEYED_STORES.iter().map(|store| store.source).collect();
    for config in KNOWN_AGENT_KEYED_CONFIG { assert!(sources.contains(config), "{config} is registered"); }
    for entry in std::fs::read_dir(home).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if name.ends_with(".db") {
            assert!(sources.contains(name.as_str()) || NOT_LABEL_KEYED_DBS.contains(&name.as_str()), "{name} is not registered");
        }
    }
    for store in sqlite_stores() {
        let db = Connection::open(home.join(store.source)).unwrap();
        let keyed = sqlite::keyed_columns(&db).unwrap();
        let tables: Vec<String> = db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
            .unwrap().query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
        for table in tables {
            let columns: Vec<String> = db.prepare("SELECT name FROM pragma_table_info(?1)").unwrap()
                .query_map([&table], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
            for column in columns {
                let lower = column.to_ascii_lowercase();
                if (lower.contains("agent") || lower == "assigned_to") && !NOT_A_LABEL.contains(&lower.as_str()) {
                    assert!(keyed.contains(&(table.clone(), column.clone())), "{}: {table}.{column} is not covered", store.source);
                }
            }
        }
    }
    for (file, table, column) in [("memory.db", "memories", "agent_id"), ("memory.db", "key_facts", "agent_id"),
        ("sessions.db", "sessions", "agent_id"), ("tasks.db", "tasks", "assigned_to"),
        ("tasks.db", "responsibilities", "owner_agent_id"), ("cron_tasks.db", "cron_tasks", "agent_id")] {
        let keyed = sqlite::keyed_columns(&Connection::open(home.join(file)).unwrap()).unwrap();
        assert!(keyed.contains(&(table.to_owned(), column.to_owned())), "{file}: {table}.{column}");
    }
}

/// S10: `rekey` moves a store once; a second call is a no-op; other labels
/// are untouched; SQLite rows end up under the quarantine key.
#[tokio::test]
async fn rekey_idempotent_per_store() {
    let dir = private_home();
    let home = dir.path();
    seed(home, LABEL).await;
    seed(home, OTHER).await;
    seed_config(home, LABEL, OTHER);
    seed_bindings(home, LABEL, OTHER);
    let key = quarantine_key(EmployeeId([9; 16]), 0);
    for store in LABEL_KEYED_STORES {
        let other_before = store.count_under(home, OTHER).unwrap();
        assert!(store.count_under(home, LABEL).unwrap() > 0, "{} holds the label", store.name);
        if store.action == TombstoneAction::Principal {
            // Never moved (a new id's own bindings may be on the label): the
            // count gates the commit, the purge removes what is left.
            assert_eq!(store.rekey(home, LABEL, &key), Ok(0), "{}", store.name);
            assert!(store.count_under(home, LABEL).unwrap() > 0);
            assert!(store.purge(home, LABEL).unwrap() > 0);
            assert_eq!(store.count_under(home, LABEL), Ok(0));
            assert_eq!(store.count_under(home, OTHER), Ok(other_before), "{}: other labels untouched", store.name);
            continue;
        }
        let moved = store.rekey(home, LABEL, &key).unwrap();
        assert!(moved > 0, "{} moved", store.name);
        assert_eq!(store.count_under(home, LABEL), Ok(0), "{}", store.name);
        assert_eq!(store.rekey(home, LABEL, &key), Ok(0), "{}: a store already moved is a no-op", store.name);
        assert_eq!(store.count_under(home, OTHER), Ok(other_before), "{}: other labels untouched", store.name);
        if matches!(store.backend, Backend::Sqlite { .. }) {
            assert_eq!(store.count_under(home, &key), Ok(moved), "{}: sealed under the row", store.name);
        }
    }
    assert_eq!(label_free(home, LABEL), Ok(true));
    assert_eq!(rekey_all(home, LABEL, &key).map(|moved| moved.iter().map(|(_, n)| n).sum::<u64>()), Ok(0));
    assert_eq!(store("memory").rekey(home, LABEL, LABEL), Err(StoreError::Unavailable), "a self re-key is refused");
}

/// S10: free only on a positive zero from every store.
#[tokio::test]
async fn label_free_requires_positive_zero_count() {
    let dir = private_home();
    let home = dir.path();
    assert_eq!(label_free(home, LABEL), Ok(true), "no store yet: nothing under the label");
    real_stores(home);
    assert_eq!(label_free(home, LABEL), Ok(true));
    let sessions = crate::session::SessionManager::new(&home.join("sessions.db")).unwrap();
    sessions.get_or_create("webchat:x", LABEL).await.unwrap();
    assert_eq!(label_free(home, LABEL), Ok(false), "one session holds the label");
    assert_eq!(label_free(home, OTHER), Ok(true));
    assert!(store("sessions").purge(home, LABEL).unwrap() >= 1);
    assert_eq!(label_free(home, LABEL), Ok(true));
}

/// S10: an unreadable store is never zero, so the label is never free.
#[test]
fn unreadable_store_is_not_zero() {
    let dir = private_home();
    let home = dir.path();
    real_stores(home);
    std::fs::remove_file(home.join("tasks.db")).unwrap();
    std::fs::write(home.join("tasks.db"), b"not a SQLite database").unwrap();
    assert_eq!(store("tasks").count_under(home, LABEL), Err(StoreError::Unavailable));
    assert_eq!(label_free(home, LABEL), Err(StoreError::Unavailable));
    assert!(counts_under(home, LABEL).iter().any(|(name, count)| *name == "tasks" && count.is_err()));
    std::fs::remove_file(home.join("tasks.db")).unwrap();
    assert_eq!(label_free(home, LABEL), Ok(true));
    // A directory where a store is expected, or a corrupt config map.
    std::fs::create_dir(home.join("cron_tasks.db.tmp")).unwrap();
    std::fs::remove_file(home.join("cron_tasks.db")).unwrap();
    std::fs::rename(home.join("cron_tasks.db.tmp"), home.join("cron_tasks.db")).unwrap();
    assert_eq!(store("cron").count_under(home, LABEL), Err(StoreError::Unavailable));
    std::fs::write(home.join("agent_bindings.json"), b"{not json").unwrap();
    assert_eq!(store("channel_routes").count_under(home, LABEL), Err(StoreError::Unavailable));
    // An unreadable lifecycle index is not "no quarantine".
    apply_db(home);
    std::fs::write(home.join(APPLY_DIR).join(APPLY_DB), b"not a SQLite database").unwrap();
    assert_eq!(label_hold(home, LABEL), Err(StoreError::Unavailable));
}

/// §3.8 entry point for F6i: purge removes every store's rows (and session
/// messages) under the label, idempotently, and nothing of another label.
#[tokio::test]
async fn purge_removes_label_stores() {
    let dir = private_home();
    let home = dir.path();
    seed(home, LABEL).await;
    seed(home, OTHER).await;
    seed_config(home, LABEL, OTHER);
    seed_bindings(home, LABEL, OTHER);
    let other_before: Vec<u64> = LABEL_KEYED_STORES.iter().map(|store| store.count_under(home, OTHER).unwrap()).collect();
    let purged = purge_all(home, LABEL).unwrap();
    assert!(purged.iter().all(|(_, removed)| *removed > 0), "{purged:?}");
    for store in LABEL_KEYED_STORES { assert_eq!(store.count_under(home, LABEL), Ok(0), "{}", store.name); }
    let other_after: Vec<u64> = LABEL_KEYED_STORES.iter().map(|store| store.count_under(home, OTHER).unwrap()).collect();
    assert_eq!(other_after, other_before);
    let messages = |session: &str| -> i64 {
        Connection::open(home.join("sessions.db")).unwrap()
            .query_row("SELECT COUNT(*) FROM session_messages WHERE session_id=?1", [session], |r| r.get(0)).unwrap()
    };
    assert_eq!(messages(&format!("webchat:{LABEL}")), 0, "child rows go with the session");
    assert_eq!(messages(&format!("webchat:{OTHER}")), 1);
    assert_eq!(label_free(home, LABEL), Ok(true));
    assert!(purge_all(home, LABEL).unwrap().iter().all(|(_, removed)| *removed == 0), "idempotent");
    // The credential shared with the other label survives without the purged one.
    let left = crate::website_credentials::list(home).unwrap();
    assert_eq!(left.iter().map(|m| (m.label.as_str(), m.allowed_agents.clone())).collect::<Vec<_>>(),
        vec![("crm", vec![OTHER.to_owned()])]);
}

/// H4 memory writer path.
#[tokio::test]
async fn memory_writer_refused_for_quarantined_label() {
    use duduclaw_core::traits::MemoryEngine;
    install_memory_gate();
    let dir = private_home();
    let home = dir.path();
    let memory = duduclaw_memory::SqliteMemoryEngine::new(&home.join("memory.db")).unwrap();
    quarantine_row(home, LABEL, 1, "sealed");
    let _mode = Mode::protected(true);
    let refused = memory.store(LABEL, memory_entry(LABEL)).await.unwrap_err();
    assert!(refused.to_string().contains(LABEL_QUARANTINED), "{refused}");
    let refused = memory.store(&format!("agent:{LABEL}"), memory_entry(LABEL)).await.unwrap_err();
    assert!(refused.to_string().contains(LABEL_QUARANTINED), "the agent:<label> namespace is the same label");
    memory.store(OTHER, memory_entry(OTHER)).await.unwrap();
    assert_eq!(store("memory").count_under(home, LABEL), Ok(0));
}

/// H4 sessions writer paths: a new session and a message append.
#[tokio::test]
async fn session_writer_refused_for_quarantined_label() {
    let dir = private_home();
    let home = dir.path();
    let sessions = crate::session::SessionManager::new(&home.join("sessions.db")).unwrap();
    sessions.get_or_create("webchat:old", LABEL).await.unwrap();
    quarantine_row(home, LABEL, 1, "sealed");
    let _mode = Mode::protected(true);
    let refused = sessions.append_message("webchat:old", "user", "planted", 1).await.unwrap_err();
    assert!(refused.to_string().contains(LABEL_QUARANTINED), "{refused}");
    // `Session` has no `Debug` (it may hold secrets): no `unwrap_err`.
    let refused = match sessions.get_or_create("webchat:new", LABEL).await {
        Ok(_) => panic!("expected the writer gate to refuse a new session"),
        Err(error) => error,
    };
    assert!(refused.to_string().contains(LABEL_QUARANTINED), "{refused}");
    sessions.get_or_create("webchat:bob", OTHER).await.unwrap();
    sessions.append_message("webchat:bob", "user", "hello", 1).await.unwrap();
    let planted: i64 = Connection::open(home.join("sessions.db")).unwrap()
        .query_row("SELECT COUNT(*) FROM session_messages WHERE session_id='webchat:old'", [], |r| r.get(0)).unwrap();
    assert_eq!(planted, 0);
}

/// H4 tasks writer path (`insert_task_tx`, shared by every task insert).
#[tokio::test]
async fn task_writer_refused_for_quarantined_label() {
    let dir = private_home();
    let home = dir.path();
    let tasks = crate::task_store::TaskStore::open(home).unwrap();
    quarantine_row(home, LABEL, 1, "sealed");
    let _mode = Mode::protected(true);
    assert_eq!(tasks.insert_task(&task_row("t1", LABEL)).await.err().as_deref(), Some(LABEL_QUARANTINED));
    tasks.insert_task(&task_row("t2", OTHER)).await.unwrap();
    assert_eq!(store("tasks").count_under(home, LABEL), Ok(0));
}

/// H4 cron writer paths: insert and a scoped edit.
#[tokio::test]
async fn cron_writer_refused_for_quarantined_label() {
    let dir = private_home();
    let home = dir.path();
    let cron = crate::cron_store::CronStore::open(home).unwrap();
    quarantine_row(home, LABEL, 1, "sealed");
    let _mode = Mode::protected(true);
    assert_eq!(cron.insert(&cron_row("c1", LABEL)).await.err().as_deref(), Some(LABEL_QUARANTINED));
    cron.insert(&cron_row("c2", OTHER)).await.unwrap();
    assert_eq!(store("cron").count_under(home, LABEL), Ok(0));
}

/// Fail closed: an unreadable lifecycle index refuses every writer; a
/// readable one without a row for the label allows it.
#[tokio::test]
async fn writer_gate_fails_closed_when_lifecycle_unreadable() {
    let dir = private_home();
    let home = dir.path();
    let tasks = crate::task_store::TaskStore::open(home).unwrap();
    apply_db(home);
    let _mode = Mode::protected(true);
    assert_eq!(check_label_writable(home, LABEL), Ok(()));
    std::fs::write(home.join(APPLY_DIR).join(APPLY_DB), b"not a SQLite database").unwrap();
    assert_eq!(check_label_writable(home, LABEL), Err(GateRefusal::Unavailable));
    assert_eq!(tasks.insert_task(&task_row("t1", OTHER)).await.err().as_deref(), Some(LABEL_STATE_UNAVAILABLE));
    assert_eq!(memory_write_gate(None, LABEL), Err(LABEL_STATE_UNAVAILABLE.to_owned()), "an unknown database file refuses");
}

/// Unprotected installs are unchanged: no gate, whatever apply.db says.
#[tokio::test]
async fn writer_gate_unchanged_outside_protected_mode() {
    let dir = private_home();
    let home = dir.path();
    let tasks = crate::task_store::TaskStore::open(home).unwrap();
    quarantine_row(home, LABEL, 1, "sealed");
    let _mode = Mode::protected(false);
    assert_eq!(check_label_writable(home, LABEL), Ok(()));
    assert_eq!(memory_write_gate(None, LABEL), Ok(()));
    tasks.insert_task(&task_row("t1", LABEL)).await.unwrap();
    assert!(channel_bot_allowed(home, LABEL, "telegram").await);
}

/// S9: channel→agent routes to the label are stripped into `stripped` at
/// tombstone (names only), other labels keep theirs, the label's schedules
/// are paused and its store marks recorded.
#[tokio::test]
async fn global_channel_route_to_label_paused() {
    let dir = private_home();
    let home = dir.path();
    seed(home, LABEL).await;
    seed_config(home, LABEL, OTHER);
    quarantine_row(home, LABEL, 1, "sealed");
    let summary = tombstone_effects(home, LABEL, EmployeeId([1; 16]), 0);
    assert_eq!(summary["fences"], json!([]), "{summary}");
    assert_eq!(summary["recorded"], true);
    assert_eq!(store("channel_routes").count_under(home, LABEL), Ok(0));
    assert_eq!(store("channel_routes").count_under(home, OTHER), Ok(1));
    let bindings = crate::agent_binding::AgentBindingStore::with_persistence(home.join("agent_bindings.json"));
    assert_eq!(bindings.resolve_bound_agent("telegram", "u1").await, None);
    assert_eq!(bindings.resolve_bound_agent("telegram", "u2").await.as_deref(), Some(OTHER));
    let row = quarantine::read_quarantine(&apply_db(home), EmployeeId([1; 16]), 0).unwrap().unwrap();
    assert_eq!(row.stripped["global_routes"]["channel_routes"], json!(["binding:telegram:u1", "token:telegram"]));
    assert_eq!(row.stripped["global_routes"]["website_credentials"], json!(["crm", "erp"]));
    assert_eq!(row.stripped["paused_work"]["cron"], json!([format!("cron-{LABEL}")]));
    let enabled: i64 = Connection::open(home.join("cron_tasks.db")).unwrap()
        .query_row("SELECT enabled FROM cron_tasks WHERE id=?1", [format!("cron-{LABEL}")], |r| r.get(0)).unwrap();
    assert_eq!(enabled, 0, "the label's schedule is paused");
    let marks: Value = serde_json::from_str(&apply_db(home).query_row(
        "SELECT store_marks FROM employee_quarantine WHERE label=?1", [LABEL], |r| r.get::<_, String>(0)).unwrap()).unwrap();
    for name in ["memory", "sessions", "tasks", "cron"] { assert!(marks[name].as_u64().unwrap() > 0, "{name}: {marks}"); }
    // A retry keeps the recorded names (nothing left to strip now).
    tombstone_effects(home, LABEL, EmployeeId([1; 16]), 0);
    let row = quarantine::read_quarantine(&apply_db(home), EmployeeId([1; 16]), 0).unwrap().unwrap();
    assert_eq!(row.stripped["global_routes"]["channel_routes"], json!(["binding:telegram:u1", "token:telegram"]));
    assert!(row.pending_fences.is_empty());
}

/// S9: other employees' delegation (org `reports_to`) pointing at the label
/// is re-rooted at tombstone and recorded; nothing else in the org changes.
#[test]
fn delegation_target_to_label_paused() {
    use duduclaw_core::org_store::{self as org, OrgEntry};
    let dir = private_home();
    let home = dir.path();
    seed_config(home, LABEL, OTHER);
    org::with_org_lock(home, |guard| org::upsert(guard, "carol-desk", OrgEntry::new(OTHER, "ops")).map(|_| ())).unwrap();
    assert_eq!(store("delegation_targets").count_under(home, LABEL), Ok(1));
    quarantine_row(home, LABEL, 1, "sealed");
    tombstone_effects(home, LABEL, EmployeeId([1; 16]), 0);
    assert_eq!(store("delegation_targets").count_under(home, LABEL), Ok(0));
    let current = org::load_for_write(home).unwrap();
    assert_eq!(current.get(OTHER), Some(&OrgEntry::new("", "sales")), "re-rooted, department kept");
    assert_eq!(current.get("carol-desk"), Some(&OrgEntry::new(OTHER, "ops")), "unrelated edge untouched");
    assert!(current.contains(LABEL), "the label's own entry is in the seal digest, not stripped");
    let row = quarantine::read_quarantine(&apply_db(home), EmployeeId([1; 16]), 0).unwrap().unwrap();
    assert_eq!(row.stripped["global_routes"]["delegation_targets"], json!([format!("reports_to:{OTHER}")]));
}

fn credential<'a>(list: &'a [crate::website_credentials::Metadata], label: &str) -> Option<&'a crate::website_credentials::Metadata> {
    list.iter().find(|m| m.label == label)
}

/// §3.3 restore fidelity: the tombstone holds the label's website grants
/// under the row key instead of deleting anything; the held grant is not
/// usable, the record cannot be deleted while held, and the exact entries
/// (credential id + grant) are in `stripped.restore`.
#[test]
fn tombstone_keeps_website_credentials_for_restore() {
    use crate::website_credentials as web;
    let dir = private_home();
    let home = dir.path();
    seed_config(home, LABEL, OTHER);
    quarantine_row(home, LABEL, 1, "sealed");
    let key = quarantine_key(EmployeeId([1; 16]), 0);
    let before = web::list(home).unwrap();
    let summary = tombstone_effects(home, LABEL, EmployeeId([1; 16]), 0);
    assert_eq!(summary["fences"], json!([]), "{summary}");
    let after = web::list(home).unwrap();
    assert_eq!(after.len(), 2, "no credential is deleted at tombstone");
    let held = vec![web::HeldGrant { agent: LABEL.to_owned(), key: key.clone() }];
    let erp = credential(&after, "erp").unwrap();
    assert!(erp.allowed_agents.is_empty(), "the only grant is held, not usable");
    assert_eq!(erp.held_grants, held);
    let crm = credential(&after, "crm").unwrap();
    assert_eq!((crm.allowed_agents.clone(), crm.held_grants.clone()), (vec![OTHER.to_owned()], held.clone()));
    assert!(web::for_agent(home, LABEL, "erp").unwrap().is_none());
    assert!(web::for_agent(home, LABEL, "crm").unwrap().is_none());
    assert!(web::for_agent(home, OTHER, "crm").unwrap().is_some(), "the other employee keeps its grant");
    assert!(web::delete(home, &erp.id).is_err(), "a held record is not deleted");
    assert_eq!(web::list(home).unwrap().len(), 2);
    assert_eq!(store("website_credentials").count_under(home, LABEL), Ok(0), "nothing under the label");
    assert_eq!(store("website_credentials").count_under(home, &key), Ok(2), "held under the row key");
    let row = quarantine::read_quarantine(&apply_db(home), EmployeeId([1; 16]), 0).unwrap().unwrap();
    let entries = row.stripped["restore"]["website_credentials"].as_array().unwrap().clone();
    let mut ids: Vec<&str> = entries.iter().map(|e| e["id"].as_str().unwrap()).collect();
    ids.sort();
    let mut expected: Vec<&str> = before.iter().map(|m| m.id.as_str()).collect();
    expected.sort();
    assert_eq!(ids, expected, "credential ids recorded");
    assert!(entries.iter().all(|e| e["agent"] == LABEL && e["key"] == key.as_str()), "{entries:?}");
    assert!(!entries.iter().any(|e| e.to_string().contains("secret-password") || e.to_string().contains("svc")),
        "no secret or username in the row");
    // A retry records nothing twice and changes nothing.
    tombstone_effects(home, LABEL, EmployeeId([1; 16]), 0);
    let row = quarantine::read_quarantine(&apply_db(home), EmployeeId([1; 16]), 0).unwrap().unwrap();
    assert_eq!(row.stripped["restore"]["website_credentials"].as_array().unwrap(), &entries);
    assert_eq!(web::list(home).unwrap(), after);
}

/// Same-id restore (F6d, §3.3/§3.7): the restore only LISTS — held website
/// grants stay held and edges stay re-rooted — and an Admin re-applies one
/// listed item at a time (`regrant_item`); an edge only where the child still
/// exists and still reports to the root; channel routes stay listed only.
#[test]
fn restore_lists_label_state_for_admin_regrant_never_auto() {
    use crate::isolation_label_restore::{regrant_item, restore_label_stores, Regranted};
    use crate::website_credentials as web;
    use duduclaw_core::org_store::{self as org, OrgEntry};
    let dir = private_home();
    let home = dir.path();
    seed_config(home, LABEL, OTHER);
    org::with_org_lock(home, |guard| {
        org::upsert(guard, "carol-desk", OrgEntry::new(LABEL, "ops"))?;
        org::upsert(guard, "dave-desk", OrgEntry::new(LABEL, "ops"))?;
        Ok(())
    }).unwrap();
    quarantine_row(home, LABEL, 1, "sealed");
    tombstone_effects(home, LABEL, EmployeeId([1; 16]), 0);
    let row = quarantine::read_quarantine(&apply_db(home), EmployeeId([1; 16]), 0).unwrap().unwrap();
    let edges = row.stripped["restore"]["delegation_targets"].as_array().unwrap();
    assert!(edges.contains(&json!({"agent": OTHER, "parent": LABEL, "department": "sales"})), "{edges:?}");
    assert_eq!(org::load_for_write(home).unwrap().get(OTHER), Some(&OrgEntry::new("", "sales")));
    // While quarantined the admin re-parents carol and removes dave.
    org::with_org_lock(home, |guard| {
        org::upsert(guard, "carol-desk", OrgEntry::new(OTHER, "ops"))?;
        org::remove(guard, "dave-desk")?;
        Ok(())
    }).unwrap();
    // Exact label only: another label's listing has nothing to re-grant.
    let wrong = restore_label_stores(home, OTHER, &row.stripped).unwrap();
    assert!(wrong.regrant.is_empty(), "{wrong:?}");
    assert!(wrong.skipped.iter().all(|s| s.ends_with(":label_mismatch")), "{wrong:?}");
    let held_before = web::list(home).unwrap();
    let org_before = org::load_for_write(home).unwrap();
    // The restore lists only: nothing is re-granted, re-parented or deleted.
    let report = restore_label_stores(home, LABEL, &row.stripped).unwrap();
    assert_eq!(report.regrant.iter().filter(|r| r.starts_with("website_credentials:")).count(), 2, "{report:?}");
    for child in [OTHER, "carol-desk", "dave-desk"] {
        assert!(report.regrant.contains(&format!("reports_to:{child}")), "{report:?}");
    }
    assert!(report.listed.contains(&"channel_routes:binding:telegram:u1".to_owned()), "{report:?}");
    assert_eq!(web::list(home).unwrap(), held_before, "held grants preserved, never re-applied automatically");
    assert!(web::for_agent(home, LABEL, "erp").unwrap().is_none());
    assert_eq!(org::load_for_write(home).unwrap().get(OTHER), org_before.get(OTHER));
    // Admin re-grant, one listed item at a time.
    let erp = credential(&held_before, "erp").unwrap().id.clone();
    assert_eq!(regrant_item(home, LABEL, &row.stripped, &format!("website_credentials:{erp}")), Ok(Regranted::Done));
    assert!(web::for_agent(home, LABEL, "erp").unwrap().is_some());
    assert!(web::for_agent(home, LABEL, "crm").unwrap().is_none(), "the other credential stays held");
    assert_eq!(regrant_item(home, LABEL, &row.stripped, &format!("website_credentials:{erp}")), Ok(Regranted::Done),
        "idempotent");
    assert_eq!(regrant_item(home, LABEL, &row.stripped, &format!("reports_to:{OTHER}")), Ok(Regranted::Done));
    assert_eq!(regrant_item(home, LABEL, &row.stripped, "reports_to:carol-desk"), Ok(Regranted::Skipped("reparented")));
    assert_eq!(regrant_item(home, LABEL, &row.stripped, "reports_to:dave-desk"), Ok(Regranted::Skipped("child_missing")));
    let current = org::load_for_write(home).unwrap();
    assert_eq!(current.get(OTHER), Some(&OrgEntry::new(LABEL, "sales")), "edge re-applied, department kept");
    assert_eq!(current.get("carol-desk"), Some(&OrgEntry::new(OTHER, "ops")), "an admin re-parent is kept");
    assert!(!current.contains("dave-desk"), "a removed child is not re-created");
    // Exact match against the listing only; channel routes have no re-grant.
    for forged in ["reports_to:mallory", "channel_routes:binding:telegram:u1", "website_credentials:", &format!("website_credentials:{erp} ")] {
        assert_eq!(regrant_item(home, LABEL, &row.stripped, forged), Ok(Regranted::NotListed), "{forged}");
    }
    assert_eq!(regrant_item(home, OTHER, &row.stripped, &format!("website_credentials:{erp}")), Ok(Regranted::NotListed));
    assert_eq!(store("channel_routes").count_under(home, LABEL), Ok(0));
    // A malformed row is refused, not guessed.
    assert_eq!(restore_label_stores(home, LABEL, &json!("not an object")), Err(StoreError::Unavailable));
}

/// F6i: purging the quarantine row deletes the website grants held for it
/// for good (a credential left with no grant goes too); a purge of the bare
/// label never touches what a row holds.
#[test]
fn purge_deletes_quarantined_website_credentials() {
    use crate::website_credentials as web;
    let dir = private_home();
    let home = dir.path();
    seed_config(home, LABEL, OTHER);
    quarantine_row(home, LABEL, 1, "sealed");
    tombstone_effects(home, LABEL, EmployeeId([1; 16]), 0);
    let key = quarantine_key(EmployeeId([1; 16]), 0);
    assert!(store("website_credentials").purge(home, LABEL).is_ok());
    assert_eq!(web::list(home).unwrap().len(), 2, "a label purge keeps the row's held grants");
    let purged = crate::isolation_label_restore::purge_row_stores(home, EmployeeId([1; 16]), 0).unwrap();
    assert!(purged.iter().any(|(name, removed)| *name == "website_credentials" && *removed == 2), "{purged:?}");
    let left = web::list(home).unwrap();
    assert_eq!(left.iter().map(|m| (m.label.as_str(), m.allowed_agents.clone(), m.held_grants.len())).collect::<Vec<_>>(),
        vec![("crm", vec![OTHER.to_owned()], 0)], "erp deleted, crm keeps the other grant");
    assert_eq!(store("website_credentials").count_under(home, &key), Ok(0));
    assert!(crate::isolation_label_restore::purge_row_stores(home, EmployeeId([1; 16]), 0).unwrap()
        .iter().all(|(_, removed)| *removed == 0), "idempotent");
}

/// R7: an open quarantine row refuses every proposal for the label except
/// the adoption of that row; a purged label frees only on a positive zero.
#[tokio::test]
async fn proposal_on_quarantined_label_refused() {
    let dir = private_home();
    let home = dir.path();
    assert_eq!(check_label_not_quarantined(home, LABEL, None), Ok(()), "no apply.db: nothing holds the label");
    quarantine_row(home, LABEL, 1, "sealed");
    assert_eq!(check_label_not_quarantined(home, LABEL, None), Err(GateRefusal::Quarantined));
    assert_eq!(proposal_refusal(home, LABEL), Err(LABEL_QUARANTINED));
    assert_eq!(check_label_not_quarantined(home, LABEL, Some((EmployeeId([1; 16]), 0))), Ok(()), "adoption of that row");
    assert_eq!(check_label_not_quarantined(home, LABEL, Some((EmployeeId([2; 16]), 0))), Err(GateRefusal::Quarantined));
    assert_eq!(check_label_not_quarantined(home, OTHER, None), Ok(()), "exact label only");
    assert_eq!(check_label_not_quarantined(home, "alice-desk-2", None), Ok(()), "never a prefix match");
    for seal in ["pending", "oversize", "invalid", "absent", "restoring", "adopting", "needs_reseal", "purging"] {
        set_seal(home, LABEL, seal);
        assert_eq!(check_label_not_quarantined(home, LABEL, None), Err(GateRefusal::Quarantined), "{seal}");
    }
    // Purged, but the purge left state behind (crash): still held (S10, §3.6).
    set_seal(home, LABEL, "purged");
    seed(home, LABEL).await;
    assert_eq!(check_label_not_quarantined(home, LABEL, None), Err(GateRefusal::Quarantined));
    purge_all(home, LABEL).unwrap();
    assert_eq!(check_label_not_quarantined(home, LABEL, None), Ok(()));
    // An unreadable store is not zero.
    std::fs::remove_file(home.join("tasks.db")).unwrap();
    std::fs::write(home.join("tasks.db"), b"not a SQLite database").unwrap();
    assert_eq!(check_label_not_quarantined(home, LABEL, None), Err(GateRefusal::Unavailable));
}

/// S7: a `restored` (or `adopted`) row is closed: it no longer blocks the
/// label for writers or proposals; `restoring` still does.
#[tokio::test]
async fn restored_row_counts_as_closed() {
    let dir = private_home();
    let home = dir.path();
    let tasks = crate::task_store::TaskStore::open(home).unwrap();
    quarantine_row(home, LABEL, 1, "restoring");
    let _mode = Mode::protected(true);
    assert_eq!(check_label_writable(home, LABEL), Err(GateRefusal::Quarantined));
    for closed in ["restored", "adopted"] {
        set_seal(home, LABEL, closed);
        let hold = label_hold(home, LABEL).unwrap();
        assert!(hold.open_rows.is_empty(), "{closed}");
        assert_eq!(check_label_writable(home, LABEL), Ok(()), "{closed}");
        assert_eq!(check_label_not_quarantined(home, LABEL, None), Ok(()), "{closed}");
    }
    tasks.insert_task(&task_row("t1", LABEL)).await.unwrap();
    // A second, open row of another id on the same label still holds it.
    quarantine_row(home, LABEL, 2, "sealed");
    assert_eq!(check_label_writable(home, LABEL), Err(GateRefusal::Quarantined));
}

/// A virtual table or one of its shadow tables (no triggers possible).
fn virtual_or_shadow(db: &Connection, table: &str) -> bool {
    let virtuals: Vec<String> = db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND sql LIKE 'CREATE VIRTUAL TABLE%'")
        .unwrap().query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
    virtuals.iter().any(|v| table == v || table.starts_with(&format!("{v}_")))
}

/// Registry-based coverage (F6fb item 2): every non-virtual table of the
/// connection that has a registered label column carries the INSERT and the
/// UPDATE gate trigger for that column, and every child table of the
/// registered parent carries the parent-lookup pair.
fn assert_every_label_table_gated(db: &Connection, parent_child: Option<&str>, exempt: &[&str]) {
    use duduclaw_memory::label_gate::{gated_tables, TRIGGER_PREFIX};
    assert_eq!(duduclaw_memory::label_gate::LABEL_COLUMNS, AGENT_COLUMNS, "one list of label columns");
    let gated = gated_tables(db).unwrap();
    let names = |table: &str| gated.iter().find(|(t, _)| t == table).map(|(_, n)| n.clone()).unwrap_or_default();
    let mut checked = 0;
    for (table, column) in sqlite::keyed_columns(db).unwrap() {
        if virtual_or_shadow(db, &table) || exempt.contains(&table.as_str()) {
            assert!(names(&table).is_empty(), "{table}: exempt tables carry no gate");
            continue;
        }
        for kind in ["i", "u"] {
            let name = format!("{TRIGGER_PREFIX}{kind}_{table}_c_{column}");
            assert!(names(&table).contains(&name), "{table}.{column}: no {kind} gate ({:?})", names(&table));
        }
        checked += 1;
    }
    assert!(checked > 0, "the store has label-keyed tables");
    if let Some(child) = parent_child {
        let tables: Vec<String> = db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
            .unwrap().query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
        for table in tables {
            let has_child: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
                [table.as_str(), child], |r| r.get(0)).unwrap();
            if !has_child || table == "sessions" || virtual_or_shadow(db, &table) { continue; }
            let name = format!("{TRIGGER_PREFIX}i_{table}_p_{child}");
            assert!(names(&table).contains(&name), "{table}.{child}: no parent gate ({:?})", names(&table));
        }
    }
}

/// F6fb item 2: every memory write that carries a label passes the gate —
/// not only `store`: the entity alias upsert, any `UPDATE memories …`
/// (content, re-key into the label), through the engine's own connection.
/// A closing write (expiry) still narrows the quarantined label's rows.
#[tokio::test]
async fn f6fb_memory_every_label_write_gated() {
    use duduclaw_core::traits::MemoryEngine;
    install_memory_gate();
    let dir = private_home();
    let home = dir.path();
    let memory = duduclaw_memory::SqliteMemoryEngine::new(&home.join("memory.db")).unwrap();
    let mine = memory_entry(LABEL);
    let id = mine.id.clone();
    memory.store(LABEL, mine).await.unwrap();
    memory.store(OTHER, memory_entry(OTHER)).await.unwrap();
    quarantine_row(home, LABEL, 1, "sealed");
    let _mode = Mode::protected(true);
    let conn = memory.conn_for_maintenance().await;
    let alias = "INSERT INTO entity_alias (agent_id, canonical, alias, created_at) VALUES (?1, 'acme', 'acme inc', 'now')
        ON CONFLICT(agent_id, alias) DO UPDATE SET canonical = excluded.canonical";
    let refused = conn.execute(alias, [LABEL]).unwrap_err();
    assert!(refused.to_string().contains(duduclaw_memory::label_gate::REFUSED), "{refused}");
    conn.execute(alias, [OTHER]).unwrap();
    assert_eq!(conn.execute("UPDATE memories SET content = 'planted' WHERE agent_id = ?1", [LABEL]).unwrap(), 0);
    assert_eq!(conn.execute("UPDATE memories SET content = 'edited' WHERE agent_id = ?1", [OTHER]).unwrap(), 1);
    assert_eq!(conn.execute("UPDATE memories SET agent_id = ?1 WHERE agent_id = ?2", [LABEL, OTHER]).unwrap(), 0,
        "nothing is re-keyed into a quarantined label");
    assert_eq!(conn.execute("UPDATE memories SET agent_id = ?1 WHERE agent_id = ?2", [OTHER, LABEL]).unwrap(), 0,
        "nothing leaves a quarantined label either");
    // Narrowing writes still run: expiry, clearing to NULL, lowering trust (never raising it).
    assert_eq!(conn.execute("UPDATE memories SET valid_until = '2026-01-01T00:00:00Z' WHERE agent_id = ?1", [LABEL]).unwrap(), 1,
        "an expiry still closes the row");
    assert_eq!(conn.execute("UPDATE memories SET supersedes = NULL WHERE agent_id = ?1", [LABEL]).unwrap(), 1);
    assert_eq!(conn.execute("UPDATE memories SET origin_trust = 0.05 WHERE agent_id = ?1", [LABEL]).unwrap(), 1);
    assert_eq!(conn.execute("UPDATE memories SET origin_trust = 0.9 WHERE agent_id = ?1", [LABEL]).unwrap(), 0);
    let content: String = conn.query_row("SELECT content FROM memories WHERE id = ?1", [&id], |r| r.get(0)).unwrap();
    assert_eq!(content, format!("{LABEL} remembers"));
    // A forget tombstone (narrowing table) is never blocked.
    assert!(duduclaw_memory::label_gate::MEMORY_NARROWING_TABLES.contains(&"forgotten_memories"));
    conn.execute("INSERT INTO forgotten_memories (memory_store, memory_id, agent_id, plan_id, forgotten_at)
        VALUES ('memories', 'gone', ?1, 'plan', 'now')", [LABEL]).unwrap();
    assert_every_label_table_gated(&conn, None, &duduclaw_memory::label_gate::MEMORY_NARROWING_TABLES);
    drop(conn);
    assert!(memory.store(LABEL, memory_entry(LABEL)).await.is_err());
}

/// F6fb item 2: sessions writers without their own check before this round
/// (failure records, compression, pinned instructions, titles) refuse, and
/// a raw write on any pool connection is gated through the session's agent.
#[tokio::test]
async fn f6fb_session_every_label_write_gated() {
    let dir = private_home();
    let home = dir.path();
    let sessions = crate::session::SessionManager::new(&home.join("sessions.db")).unwrap();
    sessions.get_or_create("webchat:old", LABEL).await.unwrap();
    sessions.append_message("webchat:old", "user", "hello", 1).await.unwrap();
    sessions.get_or_create("webchat:bob", OTHER).await.unwrap();
    quarantine_row(home, LABEL, 1, "sealed");
    let _mode = Mode::protected(true);
    let refused = sessions.record_webchat_failure("webchat:old", "boom").await.unwrap_err();
    assert!(refused.to_string().contains(LABEL_QUARANTINED), "{refused}");
    assert!(sessions.compress("webchat:old", "planted summary").await.is_err());
    assert!(sessions.set_pinned("webchat:old", "planted").await.is_err());
    assert!(sessions.set_title("webchat:old", "planted", 1).await.is_err());
    assert_eq!(sessions.get_pinned("webchat:old").await.unwrap(), "");
    assert_eq!(sessions.get_title("webchat:old").await.unwrap().0, "");
    let kept: i64 = Connection::open(home.join("sessions.db")).unwrap()
        .query_row("SELECT COUNT(*) FROM session_messages WHERE session_id = 'webchat:old' AND content = 'hello'", [], |r| r.get(0)).unwrap();
    assert_eq!(kept, 1, "compression deleted nothing");
    // Other labels are untouched by the gate.
    sessions.set_title("webchat:bob", "fine", 1).await.unwrap();
    sessions.record_webchat_failure("webchat:bob", "boom").await.unwrap();
    // The connection-level backstop on every pool connection (each guard is
    // held, so the next `acquire` returns another connection).
    let mut held = Vec::new();
    for _ in 0..4 {
        let conn = sessions.acquire().await;
        let raw = conn.execute("INSERT INTO session_messages (session_id, role, content, tokens, timestamp)
            VALUES ('webchat:old', 'assistant', 'planted', 0, 'now')", []);
        assert!(raw.unwrap_err().to_string().contains(duduclaw_memory::label_gate::REFUSED));
        assert_eq!(conn.execute("UPDATE sessions SET pinned_instructions = 'planted' WHERE id = 'webchat:old'", []).unwrap(), 0);
        assert_eq!(conn.execute("UPDATE sessions SET agent_id = ?1 WHERE id = 'webchat:bob'", [LABEL]).unwrap(), 0);
        assert_every_label_table_gated(&conn, Some("session_id"), &[]);
        held.push(conn);
    }
}

/// F6fb item 3: any closed row (purged, adopted, restored) with no live
/// confirmed row frees the label only on a positive zero count — including
/// the users.db member bindings (item 1).
#[tokio::test]
async fn f6fb_zero_count_required_for_any_closed_row() {
    for closed in ["adopted", "restored", "purged"] {
        let dir = private_home();
        let home = dir.path();
        quarantine_row(home, LABEL, 1, "sealed");
        set_seal(home, LABEL, closed);
        seed(home, LABEL).await;
        seed_bindings(home, LABEL, OTHER);
        assert_eq!(check_label_not_quarantined(home, LABEL, None), Err(GateRefusal::Quarantined), "{closed}: rows left");
        for store in LABEL_KEYED_STORES.iter().filter(|s| s.name != "member_bindings") { store.purge(home, LABEL).unwrap(); }
        assert_eq!(check_label_not_quarantined(home, LABEL, None), Err(GateRefusal::Quarantined), "{closed}: a binding left");
        assert!(store("member_bindings").purge(home, LABEL).unwrap() > 0);
        assert_eq!(check_label_not_quarantined(home, LABEL, None), Ok(()), "{closed}");
        assert_eq!(store("member_bindings").count_under(home, OTHER), Ok(1), "{closed}: other labels untouched");
        // An unreadable store is not zero.
        std::fs::remove_file(home.join("tasks.db")).unwrap();
        std::fs::write(home.join("tasks.db"), b"not a SQLite database").unwrap();
        assert_eq!(check_label_not_quarantined(home, LABEL, None), Err(GateRefusal::Unavailable), "{closed}");
    }
}
