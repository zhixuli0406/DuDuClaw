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
/// Boot and configuration reload only. A failed reload never discards a good snapshot.
pub(crate) fn reload_policy(home: &Path) {
    // Serialize reloads so an older disk read cannot overwrite a newer snapshot.
    let mut snapshots = POLICY.write().unwrap_or_else(|e|e.into_inner());
    let result = std::fs::read_to_string(home.join("config.toml"))
        .map_err(|e|e.to_string()).and_then(|s|parse_policy(&s))
        .and_then(|value| {
            let since = if home.join("users.db").exists() {
                let mut db = rusqlite::Connection::open_with_flags(home.join("users.db"),rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(|e|e.to_string())?;
                duduclaw_auth::mfa::record_enforcement(&mut db,value,chrono::Utc::now().timestamp_micros())?
            } else if value {
                return Err("MFA identity store unavailable".into());
            } else { None };
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
pub(crate) fn enforced(home: &Path, personal: bool) -> Result<bool, String> {
    if personal { return Ok(false); }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mfa_policy_snapshot_keeps_last_good_on_reload_failure() {
        let home=tempfile::tempdir().unwrap();
        let _db=UserDb::new(&home.path().join("users.db")).unwrap();
        assert!(enforced(home.path(),false).is_err());
        assert!(!enforced(home.path(),true).unwrap());
        std::fs::write(home.path().join("config.toml"),"").unwrap();
        reload_policy(home.path());
        assert!(!enforced(home.path(),false).unwrap());
        for text in ["[auth", "auth=true", "[auth]\nmfa_enforced='true'"] {
            std::fs::write(home.path().join("config.toml"),text).unwrap();
            reload_policy(home.path());
            assert!(!enforced(home.path(),false).unwrap());
            assert_eq!(policy_status(home.path())["degraded"],true);
        }
        std::fs::write(home.path().join("config.toml"),"[auth]\nmfa_enforced=true").unwrap();
        reload_policy(home.path());
        assert!(enforced(home.path(),false).unwrap());
        assert_eq!(policy_status(home.path())["degraded"],false);
        let since=policy_status(home.path())["enforced_at"].clone();
        reload_policy(home.path());
        assert_eq!(policy_status(home.path())["enforced_at"],since);
        assert!(!enforced(home.path(),true).unwrap());
        std::fs::remove_file(home.path().join("config.toml")).unwrap();
        reload_policy(home.path());
        assert!(enforced(home.path(),false).unwrap());
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
