//! RM-134 (v1.82): authentication against the hashed key store
//! (`duduclaw_core::mcp_key_store`), the recorded gateway-internal key and
//! the documented legacy `[mcp_keys]` window.

use super::*;
use duduclaw_auth::mcp_keys::{Expiry, KeyBinding, McpKeyStore, NewMcpKey};
use duduclaw_core::mcp_key_store as ks;

fn issue(home: &Path, name: &str, scopes: &[&str]) -> (String, String) {
    let created = McpKeyStore::open(home)
        .unwrap()
        .create(
            NewMcpKey {
                name: name.into(),
                client_id: format!("{name}-client"),
                binding: KeyBinding::Integration,
                env: "prod".into(),
                scopes: scopes.iter().map(|s| s.to_string()).collect(),
                expiry: Expiry::Days(30),
                created_by: Some("admin".into()),
            },
            Utc::now(),
        )
        .unwrap();
    (created.secret, created.key.id)
}

fn sql(home: &Path, statement: &str, id: &str, value: Option<String>) {
    rusqlite::Connection::open(ks::store_path(home))
        .unwrap()
        .execute(statement, rusqlite::params![id, value])
        .unwrap();
}

fn last_used(home: &Path, id: &str) -> Option<DateTime<Utc>> {
    ks::usage_map(home).unwrap().get(id).copied()
}

/// Replace the store's content, keeping a private file (so the failure is
/// the content, not the mode).
fn corrupt_store(home: &Path) {
    let path = ks::store_path(home);
    let _ = std::fs::remove_file(&path);
    drop(duduclaw_core::private_sqlite::open(home, ks::STORE_FILE, duduclaw_core::private_sqlite::Access::Create).unwrap());
    std::fs::write(path, b"corrupt store bytes, not sqlite").unwrap();
}

fn append_entry(home: &Path, key: &str, client_id: &str, scopes: &str, external: bool, created_at: &str) {
    let path = home.join("config.toml");
    let mut config = std::fs::read_to_string(&path).unwrap_or_default();
    config.push_str(&format!(
        "\n[mcp_keys.\"{key}\"]\nclient_id = \"{client_id}\"\nscopes = [{scopes}]\ncreated_at = \"{created_at}\"\nis_external = {external}\n"
    ));
    std::fs::write(path, config).unwrap();
}

#[test]
fn stored_key_authenticates_without_any_config_entry() {
    let home = TempDir::new().unwrap();
    let (raw, id) = issue(home.path(), "zapier", &["memory:read", "wiki:read"]);
    assert!(!home.path().join("config.toml").exists());
    let principal = authenticate_with_key(&raw, home.path()).unwrap();
    assert_eq!(principal.client_id, "zapier-client");
    assert!(principal.is_external);
    assert!(principal.scopes.contains(&Scope::MemoryRead) && principal.scopes.contains(&Scope::WikiRead));
    assert!(!principal.scopes.contains(&Scope::Admin));
    assert!(last_used(home.path(), &id).is_some(), "successful auth records its use in the usage file");
}

#[test]
fn expired_and_disabled_keys_refuse_with_distinct_reasons_and_one_message() {
    let home = TempDir::new().unwrap();
    let (expired, expired_id) = issue(home.path(), "old", &["memory:read"]);
    let (disabled, disabled_id) = issue(home.path(), "off", &["memory:read"]);
    sql(home.path(), "UPDATE mcp_api_keys SET expires_at = ?2 WHERE id = ?1", &expired_id,
        Some(ks::timestamp(Utc::now() - chrono::Duration::seconds(1))));
    McpKeyStore::open(home.path()).unwrap().set_disabled(&disabled_id, true, "admin", Utc::now()).unwrap();

    let e1 = authenticate_with_key(&expired, home.path()).unwrap_err();
    let e2 = authenticate_with_key(&disabled, home.path()).unwrap_err();
    assert_eq!(e1, AuthError::KeyInactive(InactiveReason::Expired));
    assert_eq!(e2, AuthError::KeyInactive(InactiveReason::Disabled));
    assert_eq!(e1.to_string(), e2.to_string(), "the caller cannot tell expired from disabled");
    assert_eq!(e1.to_string(), KEY_INACTIVE_MESSAGE);
    assert!(last_used(home.path(), &expired_id).is_none() && last_used(home.path(), &disabled_id).is_none());

    // Re-enabling is immediate (no cache).
    McpKeyStore::open(home.path()).unwrap().set_disabled(&disabled_id, false, "admin", Utc::now()).unwrap();
    assert!(authenticate_with_key(&disabled, home.path()).is_ok());
}

#[test]
fn unknown_and_deleted_keys_are_unknown() {
    let home = TempDir::new().unwrap();
    let (raw, id) = issue(home.path(), "gone", &["memory:read"]);
    assert_eq!(
        authenticate_with_key("ddc_prod_ffffffffffffffffffffffffffffffff", home.path()).unwrap_err(),
        AuthError::UnknownKey
    );
    McpKeyStore::open(home.path()).unwrap().delete(&id, "admin", Utc::now()).unwrap();
    assert_eq!(authenticate_with_key(&raw, home.path()).unwrap_err(), AuthError::UnknownKey);
    assert_eq!(authenticate_with_key("ddc_prod_short", home.path()).unwrap_err(), AuthError::InvalidFormat);
}

/// Fail closed: with an unreadable store nothing authenticates — the
/// gateway-internal key included (its acceptance needs the recorded
/// fingerprint, which lives in the store).
#[test]
fn unreadable_store_denies_every_key_including_the_internal_one() {
    let internal = fresh_key("prod");
    let home = make_config_dir_with_key(&internal, "gateway-internal", &["admin"], false, &fresh_today_rfc3339());
    let store = McpKeyStore::open(home.path()).unwrap();
    store.record_internal_key(&internal, Utc::now()).unwrap();
    drop(store);
    let (raw, _) = issue(home.path(), "zapier", &["memory:read"]);
    assert_eq!(authenticate_with_key(&internal, home.path()).unwrap().client_id, "gateway-internal");

    corrupt_store(home.path());
    assert_eq!(authenticate_with_key(&raw, home.path()).unwrap_err(), AuthError::StoreUnavailable);
    assert_eq!(authenticate_with_key(&internal, home.path()).unwrap_err(), AuthError::StoreUnavailable);
}

/// H1: an existing file without the table, with another `user_version` or
/// with a planted object is an error (deny), never "no store".
#[test]
fn a_store_without_the_table_or_with_a_wrong_schema_denies() {
    let key = fresh_key("prod");
    let home = make_config_dir_with_key(&key, "legacy", &["memory:read"], true, &fresh_today_rfc3339());
    assert!(authenticate_with_key(&key, home.path()).is_ok(), "bootstrap: no store file yet");
    let path = ks::store_path(home.path());
    for schema in [
        "CREATE TABLE other(x);".to_string(),
        ks::SCHEMA_SQL.replace("user_version = 1", "user_version = 7"),
        format!("{} CREATE TABLE planted(x);", ks::SCHEMA_SQL),
    ] {
        let _ = std::fs::remove_file(&path);
        drop(duduclaw_core::private_sqlite::open(home.path(), ks::STORE_FILE, duduclaw_core::private_sqlite::Access::Create).unwrap());
        rusqlite::Connection::open(&path).unwrap().execute_batch(&schema).unwrap();
        assert_eq!(authenticate_with_key(&key, home.path()).unwrap_err(), AuthError::StoreUnavailable, "{schema}");
    }
}

#[test]
fn a_stored_key_shadows_a_stale_config_copy() {
    let home = TempDir::new().unwrap();
    let (raw, id) = issue(home.path(), "dup", &["memory:read"]);
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "[mcp_keys.\"{raw}\"]\nclient_id = \"dup-client\"\nscopes = [\"admin\"]\ncreated_at = \"{}\"\nis_external = false\n",
            fresh_today_rfc3339()
        ),
    )
    .unwrap();
    // The store row, not the config copy, sets scopes and the external flag.
    let principal = authenticate_with_key(&raw, home.path()).unwrap();
    assert!(principal.is_external && !principal.scopes.contains(&Scope::Admin));
    McpKeyStore::open(home.path()).unwrap().set_disabled(&id, true, "admin", Utc::now()).unwrap();
    assert_eq!(authenticate_with_key(&raw, home.path()).unwrap_err(), AuthError::KeyInactive(InactiveReason::Disabled));
}

/// The bootstrap window, exactly: no `mcp_keys.db` → `[mcp_keys]` under the
/// pre-v1.82 rules (30 days); a store the gateway created but whose
/// migration it has not begun → nothing from config; pending → only the
/// recorded fingerprints; complete → nothing from config.
#[test]
fn the_legacy_window_is_exactly_bootstrap_or_recorded_pending() {
    let key = fresh_key("staging");
    let old = "ddc_dev_0000000000000000000000000000beef";
    let late = "ddc_dev_0000000000000000000000000000cafe";
    let home = make_config_dir_with_key(&key, "legacy", &["memory:read"], true, &fresh_today_rfc3339());
    append_entry(home.path(), old, "old", "\"memory:read\"", true, &(Utc::now() - chrono::Duration::days(31)).to_rfc3339());
    // 1. Bootstrap: no store file.
    assert!(authenticate_with_key(&key, home.path()).is_ok());
    assert!(matches!(authenticate_with_key(old, home.path()), Err(AuthError::KeyExpired { .. })), "30-day rule kept");
    // 2. The gateway created the store but recorded no pending set yet.
    let store = McpKeyStore::open(home.path()).unwrap();
    assert_eq!(authenticate_with_key(&key, home.path()).unwrap_err(), AuthError::UnknownKey);
    // 3. Pending: exactly the recorded set (the config as read then).
    store.begin_legacy_migration(&[ks::digest(&key)], Utc::now()).unwrap();
    assert!(authenticate_with_key(&key, home.path()).is_ok());
    append_entry(home.path(), late, "late", "\"admin\"", false, &fresh_today_rfc3339());
    assert_eq!(authenticate_with_key(late, home.path()).unwrap_err(), AuthError::UnknownKey, "added after the pending set");
    // 4. Complete: config is never consulted for a non-internal key again.
    store.clear_pending(&ks::digest(&key)).unwrap();
    assert!(store.complete_migration(Utc::now()).unwrap());
    assert_eq!(authenticate_with_key(&key, home.path()).unwrap_err(), AuthError::UnknownKey,
        "unknown fingerprint denied after the marker even though config still has it");
    assert_eq!(authenticate_with_key(late, home.path()).unwrap_err(), AuthError::UnknownKey);
}

/// H1: a config entry claiming `gateway-internal` is not the gateway's key.
#[test]
fn a_forged_gateway_internal_entry_is_denied() {
    let real = fresh_key("prod");
    let forged = "ddc_prod_0000000000000000000000000000f0f0";
    let home = make_config_dir_with_key(&real, "gateway-internal", &["admin"], false, &fresh_today_rfc3339());
    append_entry(home.path(), forged, "gateway-internal", "\"admin\"", false, &fresh_today_rfc3339());
    let store = McpKeyStore::open(home.path()).unwrap();
    store.record_internal_key(&real, Utc::now()).unwrap();
    // Before the migration even began, and after it completed.
    assert_eq!(authenticate_with_key(forged, home.path()).unwrap_err(), AuthError::UnknownKey);
    store.begin_legacy_migration(&[], Utc::now()).unwrap();
    store.complete_migration(Utc::now()).unwrap();
    assert_eq!(authenticate_with_key(forged, home.path()).unwrap_err(), AuthError::UnknownKey);
    let principal = authenticate_with_key(&real, home.path()).unwrap();
    assert_eq!(principal.client_id, "gateway-internal");
    assert!(principal.scopes.contains(&Scope::Admin));
    // The recorded key under another client id in config is not accepted.
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    append_entry(home.path(), &real, "some-employee", "\"admin\"", false, &fresh_today_rfc3339());
    assert_eq!(authenticate_with_key(&real, home.path()).unwrap_err(), AuthError::UnknownKey);
    // Removing the config entry revokes it.
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    assert_eq!(authenticate_with_key(&real, home.path()).unwrap_err(), AuthError::UnknownKey);
    // A record older than the 30-day rule refuses even with a fresh config date.
    let aged = "ddc_prod_0000000000000000000000000000aaaa";
    store.record_internal_key(aged, Utc::now() - chrono::Duration::days(31)).unwrap();
    append_entry(home.path(), aged, "gateway-internal", "\"admin\"", false, &fresh_today_rfc3339());
    assert!(matches!(authenticate_with_key(aged, home.path()), Err(AuthError::KeyExpired { .. })));
}

/// H4: authenticating never writes the authority store; usage lands in the
/// usage file, at most once a minute; a tampered usage file changes nothing.
#[test]
fn authentication_writes_only_the_usage_file() {
    let home = TempDir::new().unwrap();
    let (raw, id) = issue(home.path(), "busy", &["memory:read"]);
    let before = std::fs::read(ks::store_path(home.path())).unwrap();
    for _ in 0..5 {
        authenticate_with_key(&raw, home.path()).unwrap();
    }
    assert_eq!(std::fs::read(ks::store_path(home.path())).unwrap(), before, "authority store untouched");
    let first = last_used(home.path(), &id).unwrap();
    assert!(Utc::now().signed_duration_since(first).num_seconds() < 5);
    // The reader's own connection cannot write it either.
    let ro = ks::open_read_only(home.path()).unwrap().unwrap();
    assert!(ro.execute("UPDATE mcp_api_keys SET disabled_at = NULL", []).is_err());
    // Tampering with the usage file does not affect authentication.
    rusqlite::Connection::open(ks::usage_path(home.path()))
        .unwrap()
        .execute_batch("UPDATE mcp_key_usage SET last_used_at = 'x'; INSERT INTO mcp_key_usage VALUES ('mk_x', 'y');")
        .unwrap();
    assert!(authenticate_with_key(&raw, home.path()).is_ok());
    std::fs::write(ks::usage_path(home.path()), b"garbage").unwrap();
    // A key not stamped yet in this process really opens the (garbage) file.
    let (fresh, _) = issue(home.path(), "fresh", &["memory:read"]);
    assert!(authenticate_with_key(&fresh, home.path()).is_ok(), "an unusable usage file never denies");
    assert!(authenticate_with_key(&raw, home.path()).is_ok());
}

#[test]
fn per_call_env_authentication_reads_the_store_every_call() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().unwrap();
    let (raw, id) = issue(home.path(), "stdio", &["memory:read"]);
    let cache = KeyRegistryCache::new();
    // SAFETY: protected by ENV_LOCK — no concurrent env mutation.
    unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", &raw) };
    let first = authenticate_from_env_cached(home.path(), &cache);
    McpKeyStore::open(home.path()).unwrap().set_disabled(&id, true, "admin", Utc::now()).unwrap();
    let second = authenticate_from_env_cached(home.path(), &cache);
    unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };
    assert_eq!(first.unwrap().client_id, "stdio-client");
    assert_eq!(second.unwrap_err(), AuthError::KeyInactive(InactiveReason::Disabled), "a disable lands on the next call");
}

#[test]
fn unauthenticated_opt_in_does_not_apply_while_store_keys_exist() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().unwrap();
    issue(home.path(), "exists", &["memory:read"]);
    // SAFETY: protected by ENV_LOCK — no concurrent env mutation.
    unsafe {
        std::env::remove_var("DUDUCLAW_MCP_API_KEY");
        std::env::set_var("DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED", "1");
    }
    let result = authenticate_from_env(home.path());
    let corrupt = TempDir::new().unwrap();
    corrupt_store(corrupt.path());
    let corrupt_result = authenticate_from_env(corrupt.path());
    unsafe { std::env::remove_var("DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED") };
    assert_eq!(result.unwrap_err(), AuthError::MissingKey);
    assert_eq!(corrupt_result.unwrap_err(), AuthError::MissingKey, "an unreadable store counts as keys configured");
}
