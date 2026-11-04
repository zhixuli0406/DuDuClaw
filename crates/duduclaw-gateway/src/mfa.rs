//! MFA policy and machine encryption, shared by HTTP and dashboard RPC.
use duduclaw_auth::{UserDb, UserRole, UserStatus, mfa::MfaCipher};
use serde_json::{Value, json};
use std::path::Path;

tokio::task_local! {
    pub(crate) static ACCESS_EXPIRY: Option<u64>;
}

pub(crate) struct Cipher<'a>(pub &'a Path);
impl MfaCipher for Cipher<'_> {
    fn encrypt(&self, plaintext: &str) -> Result<String, String> {
        // Do not invoke the channel writer's legacy replace-invalid-key fallback.
        // create_new never overwrites an existing, unreadable or corrupted key.
        if !self
            .0
            .join(".keyfile")
            .try_exists()
            .map_err(|_| "無法讀取加密金鑰。")?
        {
            if self.0.join("users.db").try_exists().map_err(|_| "無法讀取帳號資料庫。")? {
                let db = rusqlite::Connection::open_with_flags(self.0.join("users.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .map_err(|_| "無法讀取帳號資料庫。")?;
                let existing: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM mfa_credentials) OR EXISTS(SELECT 1 FROM mfa_pending)", [], |r|r.get(0))
                    .map_err(|_| "無法檢查雙因素驗證資料。")?;
                if existing { return Err("雙因素驗證金鑰遺失；請還原原金鑰，或由主機操作員重設受影響帳號。".into()); }
            }
            use std::io::Write;
            let key = duduclaw_security::crypto::CryptoEngine::generate_key()
                .map_err(|_| "無法建立加密金鑰。")?;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(self.0.join(".keyfile")) {
                Ok(mut file) => {
                    file.write_all(&key)
                        .and_then(|_| file.sync_all())
                        .map_err(|_| "無法儲存加密金鑰。")?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err("無法儲存加密金鑰。".into()),
            }
        }
        let key = crate::config_crypto::load_keyfile_public(self.0).ok_or("無法讀取加密金鑰。")?;
        duduclaw_security::crypto::CryptoEngine::new(&key)
            .and_then(|c| c.encrypt_string(plaintext))
            .map_err(|_| "無法加密驗證資料。".into())
    }
    fn decrypt(&self, ciphertext: &str) -> Result<String, String> {
        crate::config_crypto::decrypt_value(ciphertext, self.0)
            .ok_or_else(|| "無法讀取雙因素驗證資料。".into())
    }
}

#[derive(Clone, Default)]
struct PolicySnapshot {
    value: Option<bool>,
    enforced_at: Option<i64>,
    degraded: bool,
}
static POLICY: std::sync::LazyLock<std::sync::RwLock<std::collections::HashMap<std::path::PathBuf, PolicySnapshot>>> =
    std::sync::LazyLock::new(Default::default);

pub(crate) fn policy_status(home: &Path) -> Value {
    let snapshots = POLICY.read().unwrap_or_else(|e|e.into_inner());
    let s = snapshots.get(home).cloned().unwrap_or(PolicySnapshot { degraded:true, ..Default::default() });
    json!({"degraded":s.degraded,"last_good_enforced":s.value,"enforced_at":s.enforced_at,
        "warning":if s.degraded {Some("雙因素驗證設定無法重新載入，目前保留最後有效設定；請檢查 config.toml。")} else {None}})
}

fn parse_policy(text: &str) -> Result<bool, String> {
    let table: toml::Table = text.parse().map_err(|_| "登入設定格式不正確。")?;
    let Some(auth) = table.get("auth") else { return Ok(false); };
    let auth = auth.as_table().ok_or("登入設定格式不正確。")?;
    match auth.get("mfa_enforced") {
        None => Ok(false),
        Some(v) => v.as_bool().ok_or_else(|| "雙因素驗證設定必須是布林值。".into()),
    }
}

/// Restore must preserve the live policy; an unreadable policy defaults to strict.
pub(crate) fn restore_policy(home: &Path) -> bool {
    // No Admin could have disabled enforcement without an identity store.
    // Edition changes never weaken the persisted policy.
    if policy_store_is_absent(home) { return true; }
    crate::handlers::config_commit::read_regular_config(&home.join("config.toml"))
        .ok().and_then(|text| {
            let table: toml::Table = text.parse().ok()?;
            if table.get("auth").and_then(|a| a.get("mfa_enforced")).is_none() {
                read_policy_state(home).ok()
            } else { parse_policy(&text).ok() }
        }).unwrap_or(true)
}

pub(crate) fn restore_policy_differs(home: &Path, text: &str) -> bool {
    parse_policy(text).ok() != Some(restore_policy(home))
}

pub(crate) const RESTORE_POLICY_NOTICE: &str =
    "還原會保留目前的雙因素驗證強制設定；如需變更，請使用雙因素驗證專用開關。";

/// Edit only this key, retaining the backup's other settings and formatting.
pub(crate) fn preserve_restore_policy(text: &str, value: bool) -> Result<String, String> {
    let mut doc = text.parse::<toml_edit::DocumentMut>().map_err(|e| e.to_string())?;
    if doc.get("auth").is_none() {
        doc["auth"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let auth = doc.get_mut("auth").and_then(toml_edit::Item::as_table_like_mut)
        .ok_or("登入設定格式不正確。")?;
    let mut replacement = toml_edit::Value::from(value);
    if let Some(old) = auth.get("mfa_enforced").and_then(toml_edit::Item::as_value) {
        *replacement.decor_mut() = old.decor().clone();
    }
    auth.insert("mfa_enforced", toml_edit::Item::Value(replacement));
    Ok(doc.to_string())
}
/// Ordinary writers must preserve the effective policy (an absent key is false).
pub(crate) fn refuse_policy_change(before: &str, after: &str) -> Result<(), String> {
    if parse_policy(before)? != parse_policy(after)? {
        return Err("auth.mfa_enforced 只能透過雙因素驗證專用開關變更（system.update_config，需 expected_state）；請保留目前設定。".into());
    }
    Ok(())
}

/// Keep reloads from reading the old config between the strict DB transition
/// and file replacement. The caller already holds the config lock.
pub(crate) fn commit_policy(
    home: &Path,
    new_content: &str,
    actor: &str,
    write: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let value = parse_policy(new_content)?;
    let mut snapshots = POLICY.write().unwrap_or_else(|e| e.into_inner());
    let snapshot = snapshots.entry(home.to_path_buf()).or_default();
    let result = (|| {
        let since = if value { record_policy(home, true)? } else { None };
        // If replacement fails after enabling, deliberately retain strict DB state.
        write()?;
        // Record the file effect before the fallible disable-side DB transition.
        crate::security_autopilot::audit_and_emit(
            home,
            &duduclaw_security::audit::AuditEvent::new(
                "config_protected_key_changed", actor,
                duduclaw_security::audit::Severity::Warning,
                json!({"key":"auth.mfa_enforced", "before":snapshot.value, "after":value,
                    "user_id":actor, "source":"system.update_config", "config_applied":true,
                    "policy_reconciled":value, "reconciling":!value}),
            ),
        );
        if !value {
            record_policy(home, false).map_err(|error| format!(
                "雙因素驗證變更已在 config.toml 生效，資料庫政策正在等待重新載入同步；同步前保留原強制政策。{error}"
            ))?;
        }
        Ok(since)
    })();
    match result {
        Ok(since) => {
            snapshot.value = Some(value);
            snapshot.enforced_at = since;
            snapshot.degraded = false;
            Ok(())
        }
        Err(error) => {
            snapshot.degraded = true;
            Err(error)
        }
    }
}

fn record_policy(home: &Path, value: bool) -> Result<Option<i64>, String> {
    if home.join("users.db").exists() {
        let mut db = rusqlite::Connection::open_with_flags(
            home.join("users.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        ).map_err(|e| e.to_string())?;
        duduclaw_auth::mfa::record_enforcement(&mut db, value, chrono::Utc::now().timestamp_micros())
    } else if value {
        Err("MFA identity store unavailable".into())
    } else {
        Ok(None)
    }
}

/// Read the durable boundary without creating/migrating the identity store.
/// This reader is strict; boot_policy separately handles a genuinely absent store.
fn read_policy_state(home: &Path) -> Result<bool, String> {
    let db = rusqlite::Connection::open_with_flags(
        home.join("users.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ).map_err(|e| e.to_string())?;
    duduclaw_auth::mfa::read_enforcement(&db)
}

fn read_boot_policy_state(home: &Path) -> Result<bool, String> {
    let path = home.join("users.db");
    if !std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?.file_type().is_file() {
        return Err("users.db must be a regular file".into());
    }
    let db = rusqlite::Connection::open_with_flags(
        path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ).map_err(|e| e.to_string())?;
    match duduclaw_auth::mfa::read_enforcement(&db) {
        Ok(value) => Ok(value),
        Err(message) if message == "no such table: mfa_policy_state" => {
            // Only a wholly pre-MFA schema may pass without a recorded policy.
            let legacy: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND lower(name)='users')
                 AND NOT EXISTS(SELECT 1 FROM sqlite_master WHERE type IN ('table','view') AND lower(name) GLOB 'mfa_*')",
                [], |r| r.get(0),
            ).map_err(|e| e.to_string())?;
            if legacy { Ok(false) } else { Err(message.clone()) }
        }
        Err(error) => Err(error.to_string()),
    }
}

/// Boot precheck runs before UserDb migration. Accept only the released
/// unseeded state here, without weakening readers used by config/import gates.
pub(crate) fn precheck_boot_policy(home: &Path, personal: bool) -> Result<bool, String> {
    if matches!(read_boot_policy_state(home), Err(error) if error == "MFA policy singleton missing") {
        let db = rusqlite::Connection::open_with_flags(
            home.join("users.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ).map_err(|e| e.to_string())?;
        // Project both columns so an empty but malformed table is still refused.
        let mut statement = db.prepare("SELECT id, enforced FROM mfa_policy_state")
            .map_err(|e| e.to_string())?;
        let empty = statement.query([]).map_err(|e| e.to_string())?
            .next().map_err(|e| e.to_string())?.is_none();
        let legacy: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND lower(name)='users')
             AND EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND lower(name)='mfa_policy_state')",
            [], |r| r.get(0),
        ).map_err(|e| e.to_string())?;
        if empty && legacy { return Ok(false); }
        return Err("MFA policy state is not a legacy unseeded store".into());
    }
    boot_policy(home, personal)
}

pub(crate) fn boot_policy(home: &Path, personal: bool) -> Result<bool, String> {
    // An absent directory entry needs no policy read.
    // A dangling symlink or any other existing but unreadable store must fail closed.
    if policy_store_is_absent(home) {
        return Ok(!personal);
    }
    match read_boot_policy_state(home) {
        Ok(value) => Ok(value),
        Err(error) => {
            POLICY.write().unwrap_or_else(|e| e.into_inner())
                .entry(home.to_path_buf()).or_default().degraded = true;
            Err(format!("MFA policy state unavailable; refusing config provisioning: {error}"))
        }
    }
}

pub(crate) fn policy_store_is_absent(home: &Path) -> bool {
    matches!(std::fs::symlink_metadata(home.join("users.db")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}

/// A missing auth key is not permission for an ordinary writer to disable
/// the durable policy by adding false. Only the Admin MFA adapter may do so.
pub(crate) fn refuse_policy_change_at(home: &Path, before: &str, after: &str) -> Result<(), String> {
    refuse_policy_change(before, after)?;
    // Even an unchanged explicit key must not let an ordinary save repair
    // a corrupt store via the subsequent policy reload.
    if !policy_store_is_absent(home) { read_boot_policy_state(home)?; }
    let before_table: toml::Table = before.parse().map_err(|_| "登入設定格式不正確。")?;
    let after_table: toml::Table = after.parse().map_err(|_| "登入設定格式不正確。")?;
    if before_table.get("auth").and_then(|a| a.get("mfa_enforced")).is_none()
        && after_table.get("auth").and_then(|a| a.get("mfa_enforced")).and_then(|v| v.as_bool()) == Some(false)
        && read_policy_state(home)?
    {
        return Err("auth.mfa_enforced 只能透過雙因素驗證專用開關變更。".into());
    }
    Ok(())
}

/// Imports reject policy changes before replacing any data, including users.db.
/// Unlike restore, the caller supplies the verified edition.
pub(crate) fn refuse_import_policy_change(home: &Path, after: &str, personal: bool, replaces_identity: bool) -> Result<String, String> {
    let before = match crate::handlers::config_commit::read_regular_config(&home.join("config.toml")) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.to_string()),
    };
    let configured = parse_policy(&before)?;
    let table: toml::Table = before.parse().map_err(|_| "登入設定格式不正確。")?;
    let absent = policy_store_is_absent(home);
    // Always validate an existing store, even with an explicit false key.
    let fallback = if absent { !personal } else { read_boot_policy_state(home)? };
    let current = if (absent && !personal)
        || table.get("auth").and_then(|a| a.get("mfa_enforced")).is_none() { fallback }
        else { configured || (!absent && fallback) };
    let parsed_after = parse_policy(after)?;
    let after_table: toml::Table = after.parse().map_err(|_| "登入設定格式不正確。")?;
    let missing_key = after_table.get("auth").and_then(|a| a.get("mfa_enforced")).is_none();
    if !missing_key && !parsed_after && fallback {
        return Err("auth.mfa_enforced 只能透過雙因素驗證專用開關變更（system.update_config，需 expected_state）；請保留目前設定。".into());
    }
    // Personal may replace identity without an explicit key, but the imported
    // config must carry the live policy so reload cannot trust the archived DB.
    if personal && replaces_identity && missing_key {
        return if current || fallback { preserve_restore_policy(after, true) }
            else { Ok(after.to_owned()) };
    }
    let next = if !replaces_identity && missing_key { fallback }
        else { parsed_after };
    if current != next {
        return Err("auth.mfa_enforced 只能透過雙因素驗證專用開關變更（system.update_config，需 expected_state）；請保留目前設定。".into());
    }
    if personal && missing_key && current {
        preserve_restore_policy(after, true)
    } else {
        Ok(after.to_owned())
    }
}

pub(crate) fn audit_policy_restored(home: &Path) {
    crate::security_autopilot::audit_and_emit(home,
        &duduclaw_security::audit::AuditEvent::new(
            "mfa.policy_restored_from_state", "gateway_boot",
            duduclaw_security::audit::Severity::Warning,
            json!({"mfa_enforced":true,"source":"mfa_policy_state"}),
        ));
}

/// Boot and configuration reload only. A failed reload never discards a good snapshot.
pub(crate) fn reload_policy(home: &Path) {
    // Serialize reloads so an older disk read cannot overwrite a newer snapshot.
    let mut snapshots = POLICY.write().unwrap_or_else(|e|e.into_inner());
    let result = crate::handlers::config_commit::read_regular_config(&home.join("config.toml"))
        .map_err(|e|e.to_string()).and_then(|s| {
            let table: toml::Table = s.parse().map_err(|_| "登入設定格式不正確。".to_string())?;
            // Only an explicit boolean false can reconcile an Admin disable.
            // An absent auth table/key must preserve the durable boundary.
            if table.get("auth").is_none_or(|a| a.as_table().is_some_and(|a| !a.contains_key("mfa_enforced"))) {
                read_policy_state(home)
            } else {
                parse_policy(&s)
            }
        })
        .and_then(|value| {
            let since = record_policy(home, value)?;
            Ok((value,since))
        });
    let snapshot = snapshots.entry(home.to_path_buf()).or_default();
    match result {
        Ok((value,since)) => { snapshot.value=Some(value); snapshot.enforced_at=since; snapshot.degraded=false; }
        Err(error) => {
            snapshot.degraded=true;
            tracing::warn!(%error,"MFA policy reload failed; retaining last valid snapshot (degraded)");
        }
    }
}
pub(crate) fn enforced(home: &Path, _personal: bool) -> Result<bool, String> {
    // Edition only determines the fresh-install default, never runtime enforcement.
    POLICY.read().unwrap_or_else(|e|e.into_inner()).get(home).and_then(|s|s.value)
        .ok_or_else(|| "尚無有效的登入設定；請檢查 config.toml。".into())
}
pub(crate) fn require_admin(db: &UserDb, id: &str) -> Result<(), String> {
    match db.get_user(id)? {
        Some(u) if u.role == UserRole::Admin && u.status == UserStatus::Active => Ok(()),
        _ => Err("需要目前的管理員權限。".into()),
    }
}
pub(crate) fn enrollment_payload(secret: String, email: &str) -> Value {
    let label: String =
        url::form_urlencoded::byte_serialize(format!("DuDuClaw:{email}").as_bytes()).collect();
    json!({"secret":secret,"otpauth_uri":format!("otpauth://totp/{label}?secret={secret}&issuer=DuDuClaw&algorithm=SHA1&digits=6&period=30")})
}

/// Called inside the MFA write transaction. No token-issuance failure can
/// commit enrollment without its recovery codes or commit a partial disable.
pub(crate) fn replacement_session(
    jwt: &duduclaw_auth::JwtConfig,
    user: &duduclaw_auth::User,
    access: &[(String, duduclaw_auth::AccessLevel)],
    previous_epoch: u64,
    codes: Vec<String>,
    enabled: bool,
) -> Result<serde_json::Value, String> {
    if previous_epoch.checked_add(1) != Some(user.credential_epoch) {
        return Err("登入憑證已失效，請重新登入。".into());
    }
    let (access_token, refresh_token) = jwt.issue_session_tokens(user, access)?;
    Ok(serde_json::json!({
        "enabled": enabled, "recovery_codes": codes,
        "access_token": access_token, "refresh_token": refresh_token, "user": user,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mfa_import_preserves_persisted_policy_across_edition_identity_and_archive_key() {
        let home = tempfile::tempdir().unwrap();
        let config = "[auth]\nmfa_enforced=true\n";
        std::fs::write(home.path().join("config.toml"), config).unwrap();
        drop(UserDb::new(&home.path().join("users.db")).unwrap());
        let mut conn = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
        duduclaw_auth::mfa::record_enforcement(&mut conn, true, 123).unwrap();
        for personal in [true, false] {
            for replaces_identity in [false, true] {
                for key in [None, Some(true), Some(false)] {
                    let after = key.map(|value| format!("[auth]\nmfa_enforced={value}\n")).unwrap_or_default();
                    let result = refuse_import_policy_change(home.path(), &after, personal, replaces_identity);
                    let allowed = key == Some(true) || (key.is_none() && (personal || !replaces_identity));
                    assert_eq!(result.is_ok(), allowed, "personal={personal}, replaces_identity={replaces_identity}, key={key:?}: {result:?}");
                    if let Ok(prepared) = result {
                        if personal && key.is_none() {
                            assert!(parse_policy(&prepared).unwrap());
                        }
                    }
                }
            }
        }
        assert!(refuse_import_policy_change(home.path(), "[auth]\nmfa_enforced='false'", true, true).is_err());
        assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), config);
        assert!(duduclaw_auth::mfa::read_enforcement(&conn).unwrap());
    }

    #[test]
    fn mfa_keyless_agent_only_import_allows_policy_increase_only() {
        for configured in [true, false] {
            let home = tempfile::tempdir().unwrap();
            std::fs::write(home.path().join("config.toml"), format!("[auth]\nmfa_enforced={configured}\n")).unwrap();
            drop(UserDb::new(&home.path().join("users.db")).unwrap());
            let mut conn = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
            duduclaw_auth::mfa::record_enforcement(&mut conn, !configured, 123).unwrap();
            for personal in [true, false] {
                let result = refuse_import_policy_change(home.path(), "", personal, false);
                if configured {
                    assert!(result.is_err());
                } else {
                    let prepared = result.unwrap();
                    if personal {
                        assert!(parse_policy(&prepared).unwrap());
                    }
                }
            }
        }
    }

    #[test]
    fn mfa_import_never_lowers_durable_true_even_with_explicit_live_false() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[auth]\nmfa_enforced=false\n").unwrap();
        drop(UserDb::new(&home.path().join("users.db")).unwrap());
        let mut conn = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
        duduclaw_auth::mfa::record_enforcement(&mut conn, true, 123).unwrap();
        for personal in [true, false] {
            for replaces_identity in [true, false] {
                assert!(refuse_import_policy_change(home.path(), "[auth]\nmfa_enforced=false\n", personal, replaces_identity).is_err());
            }
        }
        let prepared = refuse_import_policy_change(home.path(), "", true, true).unwrap();
        assert!(parse_policy(&prepared).unwrap());
        assert!(duduclaw_auth::mfa::read_enforcement(&conn).unwrap());
    }

    #[test]
    fn mfa_import_degraded_config_false_durable_true_uses_durable_floor() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[auth]\nmfa_enforced=false\n").unwrap();
        drop(UserDb::new(&home.path().join("users.db")).unwrap());
        let mut conn = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
        duduclaw_auth::mfa::record_enforcement(&mut conn, true, 123).unwrap();

        for personal in [true, false] {
            for replaces_identity in [true, false] {
                assert!(refuse_import_policy_change(home.path(), "[auth]\nmfa_enforced=true\n", personal, replaces_identity).is_ok());
                assert!(refuse_import_policy_change(home.path(), "[auth]\nmfa_enforced=false\n", personal, replaces_identity).is_err());
            }
        }
        // Keyless imports follow the pre-existing table: identity replacement
        // preserves the durable policy for personal imports; other imports refuse.
        assert!(parse_policy(&refuse_import_policy_change(home.path(), "", true, true).unwrap()).unwrap());
        let prepared = refuse_import_policy_change(home.path(), "", true, false).unwrap();
        assert!(parse_policy(&prepared).unwrap());
        assert!(refuse_import_policy_change(home.path(), "", false, true).is_err());
        assert!(refuse_import_policy_change(home.path(), "", false, false).is_ok());
    }

    #[test]
    fn boot_policy_legacy_store_allows_migration_then_reads_durable_policy() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("users.db");
        let mut conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE users (
            id TEXT PRIMARY KEY, email TEXT UNIQUE NOT NULL, display_name TEXT NOT NULL,
            password_hash TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'employee',
            status TEXT NOT NULL DEFAULT 'active', created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL, last_login TEXT);").unwrap();
        assert!(!boot_policy(home.path(), false).unwrap());
        assert!(read_policy_state(home.path()).is_err());
        assert!(refuse_policy_change_at(home.path(), "", "[auth]\nmfa_enforced=false").is_err());
        crate::mcp_internal_key::ensure_internal_mcp_key_for_gateway(
            home.path(), "127.0.0.1:18789".parse().unwrap(), false, None,
        ).unwrap();
        assert!(read_policy_state(home.path()).is_err());
        let _db = UserDb::new(&path).unwrap();
        reload_policy(home.path());
        assert!(!read_policy_state(home.path()).unwrap());
        assert!(!conn.query_row("SELECT enforced FROM mfa_policy_state WHERE id=1", [], |r| r.get::<_, bool>(0)).unwrap());
        assert!(!boot_policy(home.path(), false).unwrap());
        duduclaw_auth::mfa::record_enforcement(&mut conn, true, 123).unwrap();
        assert!(boot_policy(home.path(), false).unwrap());
        conn.execute_batch("DROP TABLE mfa_policy_state").unwrap();
        assert!(boot_policy(home.path(), false).is_err());
        assert_eq!(policy_status(home.path())["degraded"], true);
    }

    #[test]
    fn mfa_explicit_false_reload_cannot_repair_missing_singleton() {
        let home = tempfile::tempdir().unwrap();
        drop(UserDb::new(&home.path().join("users.db")).unwrap());
        std::fs::write(home.path().join("config.toml"), "[auth]\nmfa_enforced=true\n").unwrap();
        reload_policy(home.path());
        assert!(enforced(home.path(), false).unwrap());
        let conn = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
        conn.execute("DELETE FROM mfa_policy_state", []).unwrap();
        std::fs::write(home.path().join("config.toml"), "[auth]\nmfa_enforced=false\n").unwrap();
        // This is a live-store deletion after initialization, not an upgrade.
        // Reload must not run migration or repair it, even with explicit false.
        assert!(read_policy_state(home.path()).is_err());
        assert!(read_boot_policy_state(home.path()).is_err());
        assert!(refuse_policy_change_at(home.path(), "[auth]\nmfa_enforced=false", "[auth]\nmfa_enforced=false").is_err());
        assert!(refuse_import_policy_change(home.path(), "[auth]\nmfa_enforced=false", false, true).is_err());
        reload_policy(home.path());
        assert!(enforced(home.path(), false).unwrap());
        assert_eq!(policy_status(home.path())["degraded"], true);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM mfa_policy_state", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    #[test]
    fn mfa_invalid_singleton_refuses_boot_and_config_write() {
        for rows in ["", "(1,0),(1,0)", "(1,2)", "(1,'false')", "(1,NULL)", "(2,0)"] {
            let home = tempfile::tempdir().unwrap();
            let conn = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
            conn.execute_batch("CREATE TABLE users(id TEXT); CREATE TABLE mfa_policy_state(id, enforced);").unwrap();
            if !rows.is_empty() {
                conn.execute_batch(&format!("INSERT INTO mfa_policy_state VALUES {rows}")).unwrap();
            }
            if rows.is_empty() {
                assert!(!precheck_boot_policy(home.path(), false).unwrap());
            } else {
                assert!(precheck_boot_policy(home.path(), false).is_err(), "{rows}");
            }
            assert!(read_policy_state(home.path()).is_err(), "{rows}");
            assert!(read_boot_policy_state(home.path()).is_err(), "{rows}");
            assert!(boot_policy(home.path(), false).is_err(), "{rows}");
            assert_eq!(policy_status(home.path())["degraded"], true);
            assert!(refuse_policy_change_at(home.path(), "", "[auth]\nmfa_enforced=false").is_err(), "{rows}");
        }
    }

    #[test]
    fn boot_policy_partial_or_invalid_store_refuses_and_degrades() {
        for schema in [
            "CREATE TABLE users(id TEXT); CREATE TABLE mfa_credentials(id TEXT);",
            "CREATE TABLE users(id TEXT); CREATE TABLE mfa_custom(id TEXT);",
            "CREATE TABLE unrelated(id TEXT);",
            "CREATE TABLE users(id TEXT); CREATE TABLE mfa_policy_state(id INTEGER);",
        ] {
            let home = tempfile::tempdir().unwrap();
            let conn = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
            conn.execute_batch(schema).unwrap();
            assert!(boot_policy(home.path(), false).is_err(), "{schema}");
            assert!(boot_policy(home.path(), true).is_err(), "{schema}");
            assert_eq!(policy_status(home.path())["degraded"], true);
        }
        for bytes in [b"".as_slice(), b"not a sqlite database".as_slice()] {
            let home = tempfile::tempdir().unwrap();
            std::fs::write(home.path().join("users.db"), bytes).unwrap();
            assert!(boot_policy(home.path(), false).is_err());
            assert_eq!(policy_status(home.path())["degraded"], true);
            assert!(boot_policy(home.path(), true).is_err());
        }
    }

    #[test]
    fn boot_policy_refuses_legacy_users_schema_with_mfa_view() {
        let home = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
        conn.execute_batch("CREATE TABLE users(id TEXT); CREATE VIEW mfa_x AS SELECT id FROM users;").unwrap();
        assert!(boot_policy(home.path(), false).is_err());
        assert_eq!(policy_status(home.path())["degraded"], true);
    }

    #[cfg(unix)]
    #[test]
    fn boot_policy_symlink_to_legacy_store_refuses() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("legacy.db");
        let conn = rusqlite::Connection::open(&target).unwrap();
        conn.execute_batch("CREATE TABLE users(id TEXT)").unwrap();
        std::os::unix::fs::symlink(target, home.path().join("users.db")).unwrap();
        assert!(boot_policy(home.path(), false).is_err());
        assert_eq!(policy_status(home.path())["degraded"], true);
    }

    #[test]
    fn mfa_restore_policy_defaults_strict_when_unreadable() {
        let home = tempfile::tempdir().unwrap();
        assert!(restore_policy(home.path()));
        std::fs::create_dir(home.path().join("config.toml")).unwrap();
        assert!(restore_policy(home.path()));
    }

    #[cfg(unix)]
    #[test]
    fn mfa_restore_missing_or_unreadable_store_is_strict() {
        let home = tempfile::tempdir().unwrap();
        let config = "[gateway]\nport = 18789\n";
        std::fs::write(home.path().join("config.toml"), config).unwrap();
        assert!(restore_policy(home.path()));
        assert!(restore_policy_differs(home.path(), config));

        let db = home.path().join("users.db");
        std::fs::write(&db, b"not a sqlite database").unwrap();
        std::fs::set_permissions(&db, std::os::unix::fs::PermissionsExt::from_mode(0)).unwrap();
        assert!(restore_policy(home.path()));
    }

    #[test]
    fn mfa_restore_rewrites_only_key_in_inline_auth_table() {
        let before = "auth = { mfa_enforced = false, other = 'keep' } # comment\n";
        let after = preserve_restore_policy(before, true).unwrap();
        assert_eq!(after, before.replacen("false", "true", 1));
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[auth]\nmfa_enforced=true").unwrap();
        assert!(restore_policy_differs(home.path(), before));
        assert!(!restore_policy_differs(home.path(), &after));
    }

    #[test]
    fn mfa_ordinary_writes_compare_effective_parsed_values() {
        for (before, after) in [
            ("", "[auth]\nmfa_enforced=true"),
            ("[auth]\nmfa_enforced=true", ""),
            ("[auth]\nmfa_enforced=true", "[auth]"),
            ("[auth]\nmfa_enforced=true", "auth=false"),
        ] {
            assert!(refuse_policy_change(before, after).is_err());
        }
        assert!(refuse_policy_change("", "[auth]\nmfa_enforced=false").is_ok());
        assert!(refuse_policy_change("[auth]\nmfa_enforced=true", "# comment\n[auth]\nmfa_enforced = true\n[general]\nlog_level='debug'").is_ok());
    }

    #[test]
    fn mfa_policy_snapshot_keeps_last_good_on_reload_failure() {
        let home=tempfile::tempdir().unwrap();
        let _db=UserDb::new(&home.path().join("users.db")).unwrap();
        assert!(enforced(home.path(),false).is_err());
        assert!(enforced(home.path(),true).is_err());
        std::fs::write(home.path().join("config.toml"),"").unwrap();
        reload_policy(home.path());
        assert!(!enforced(home.path(),false).unwrap());
        for text in ["[auth", "auth=true", "[auth]\nmfa_enforced='true'"] {
            std::fs::write(home.path().join("config.toml"),text).unwrap();
            reload_policy(home.path());
            assert!(!enforced(home.path(),false).unwrap());
            assert!(!enforced(home.path(),true).unwrap());
            assert_eq!(policy_status(home.path())["degraded"],true);
        }
        std::fs::write(home.path().join("config.toml"),"[auth]\nmfa_enforced=true").unwrap();
        reload_policy(home.path());
        assert!(enforced(home.path(),false).unwrap());
        assert_eq!(policy_status(home.path())["degraded"],false);
        let since=policy_status(home.path())["enforced_at"].clone();
        reload_policy(home.path());
        assert_eq!(policy_status(home.path())["enforced_at"],since);
        assert!(enforced(home.path(),true).unwrap());
        std::fs::remove_file(home.path().join("config.toml")).unwrap();
        reload_policy(home.path());
        assert!(enforced(home.path(),false).unwrap());
    }
    #[test]
    fn missing_auth_preserves_durable_enforcement_and_unreadable_state_degrades() {
        let home = tempfile::tempdir().unwrap();
        let _db = UserDb::new(&home.path().join("users.db")).unwrap();
        let mut conn = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
        duduclaw_auth::mfa::record_enforcement(&mut conn, true, 123).unwrap();
        for text in ["[gateway]\nport=18789\n", "[auth]\n"] {
            std::fs::write(home.path().join("config.toml"), text).unwrap();
            reload_policy(home.path());
            assert!(enforced(home.path(), false).unwrap());
            assert!(restore_policy(home.path()));
            assert_eq!(policy_status(home.path())["enforced_at"], 123);
            assert!(refuse_policy_change_at(home.path(), text, "[auth]\nmfa_enforced=false").is_err());
        }
        conn.execute_batch("DROP TABLE mfa_policy_state").unwrap();
        reload_policy(home.path());
        assert_eq!(policy_status(home.path())["degraded"], true);
        assert!(enforced(home.path(), false).unwrap());
        let fresh = tempfile::tempdir().unwrap();
        std::fs::write(fresh.path().join("config.toml"), "# no auth\n").unwrap();
        reload_policy(fresh.path());
        assert_eq!(policy_status(fresh.path())["degraded"], true);
        assert!(enforced(fresh.path(), false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_false_cannot_disable_policy_on_reload() {
        let home = tempfile::tempdir().unwrap();
        let _db = UserDb::new(&home.path().join("users.db")).unwrap();
        let mut conn = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
        duduclaw_auth::mfa::record_enforcement(&mut conn, true, 123).unwrap();
        std::fs::write(home.path().join("config.toml"), "[auth]\nmfa_enforced=true").unwrap();
        reload_policy(home.path());
        std::fs::remove_file(home.path().join("config.toml")).unwrap();
        let target = home.path().join("false.toml");
        std::fs::write(&target, "[auth]\nmfa_enforced=false").unwrap();
        std::os::unix::fs::symlink(target, home.path().join("config.toml")).unwrap();
        reload_policy(home.path());
        assert!(enforced(home.path(), false).unwrap());
        assert!(restore_policy(home.path()));
        assert_eq!(policy_status(home.path())["degraded"], true);
        let durable: bool = conn.query_row("SELECT enforced FROM mfa_policy_state WHERE id=1", [], |r| r.get(0)).unwrap();
        assert!(durable);
    }

    #[test]
    fn mfa_missing_key_with_rows_never_generates_replacement() {
        let home=tempfile::tempdir().unwrap();
        let _db=UserDb::new(&home.path().join("users.db")).unwrap();
        let raw=rusqlite::Connection::open(home.path().join("users.db")).unwrap();
        raw.execute("INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES('u','lost',0)",[]).unwrap();
        assert!(Cipher(home.path()).encrypt("secret").unwrap_err().contains("金鑰遺失"));
        assert!(!home.path().join(".keyfile").exists());
    }
    #[test]
    fn mfa_cipher_roundtrip_corruption_never_replaces_key() {
        let home = tempfile::tempdir().unwrap();
        let cipher = Cipher(home.path());
        let encrypted = cipher.encrypt("secret").unwrap();
        assert_ne!(encrypted, "secret");
        assert_eq!(cipher.decrypt(&encrypted).unwrap(), "secret");
        std::fs::write(home.path().join(".keyfile"), b"bad").unwrap();
        assert!(cipher.encrypt("secret").is_err());
        assert!(cipher.decrypt(&encrypted).is_err());
        assert_eq!(std::fs::read(home.path().join(".keyfile")).unwrap(), b"bad");
    }
}
